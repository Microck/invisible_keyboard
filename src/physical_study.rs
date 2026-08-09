//! Leakage-resistant scoring for the first physical sensing studies.
//!
//! The engine deliberately does not know how to collect sensor data. It accepts
//! append-only, self-hashed JSONL evidence rows, fits only a fixed nearest-
//! centroid reference model on `train`, selects one abstention threshold on
//! `validation`, and evaluates `heldout`. Gate 0B consumes the already-frozen
//! contact detector events directly. This keeps the eventual hardware study
//! executable without pretending that generated observations prove the sensor.

use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt,
    io::{self, Write},
};

use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::{
    layout::{Hand, all_slots, slot_for_label},
    manifest::{format_sha256, parse_sha256, sha256},
};

/// Schema for each independently hashed JSONL evidence row.
pub const PHYSICAL_STUDY_EVIDENCE_SCHEMA: &str =
    "invisible-keyboard-physical-study-evidence-row-v1";

/// Schema for the deterministic scorecard emitted by this module.
pub const PHYSICAL_STUDY_REPORT_SCHEMA: &str = "invisible-keyboard-physical-study-report-v1";

/// Schema for the frozen, pre-collection row roster and split ownership.
pub const PHYSICAL_STUDY_SPLIT_MANIFEST_SCHEMA: &str =
    "invisible-keyboard-physical-study-split-manifest-v1";

/// Schema for the frozen quantitative analysis contract.
pub const PHYSICAL_STUDY_ANALYSIS_PLAN_SCHEMA: &str =
    "invisible-keyboard-physical-study-analysis-plan-v1";

/// Maximum exact evidence or roster-template JSONL accepted or emitted by the library.
/// The minimum valid Gate 2 study is far smaller, leaving room for longer IDs,
/// provenance, and richer frozen derived feature vectors.
pub const PHYSICAL_STUDY_EVIDENCE_JSONL_MAX_BYTES: usize = 256 * 1024 * 1024;

/// Maximum encoded JSON bytes for one evidence row, excluding its newline.
/// Large raw sensor windows belong in the hashed observation artifact, not in
/// an inline feature vector.
pub const PHYSICAL_STUDY_EVIDENCE_ROW_MAX_BYTES: usize = 1024 * 1024;

/// Maximum exact split-manifest JSON accepted or emitted by the library.
pub const PHYSICAL_STUDY_SPLIT_MANIFEST_MAX_BYTES: usize = 64 * 1024 * 1024;

/// Maximum exact analysis-plan JSON accepted or emitted by the library.
pub const PHYSICAL_STUDY_ANALYSIS_PLAN_MAX_BYTES: usize = 64 * 1024;

/// Maximum canonical physical-study report JSON accepted or emitted by the library.
pub const PHYSICAL_STUDY_REPORT_MAX_BYTES: usize = 16 * 1024 * 1024;

const CENTROID_MODEL_SCHEMA: &str = "invisible-keyboard-physical-study-centroid-model-v1";
const MATCH_BEFORE_US: u64 = 50_000;
const MATCH_AFTER_US: u64 = 250_000;
const MIN_EVENTS_PER_CLASS_PER_MOUNT: u64 = 50;
const MIN_NEGATIVE_EXPOSURE_US_PER_CONDITION: u64 = 60_000_000;
const BOOTSTRAP_REPLICATES: usize = 20_000;
const RATE_SCALE: u64 = 1_000_000;
const MIN_CONTACT_LATENCY_US: i64 = 0;
const MISS_UPPER_LIMIT_PPM: u32 = 50_000;
const EVENT_ERROR_UPPER_LIMIT_PPM: u32 = 100_000;
const MIN_RECALL_PPM: u32 = 800_000;
const MIN_MACRO_F1_PPM: u32 = 900_000;
const ONE_SIDED_ALPHA_PPM: u32 = 50_000;
const LATENCY_P95_LIMIT_US: i64 = 100_000;
const LATENCY_P99_LIMIT_US: i64 = 200_000;

/// One supported physical feasibility gate.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum PhysicalStudyGate {
    /// Contact observability and timing, before finger classification.
    #[serde(rename = "gate-0b")]
    Gate0B,
    /// Acting-finger identity with a frozen contact detector.
    #[serde(rename = "gate-1")]
    Gate1,
    /// Exact Core-28 physical position, including reserved-position abstention.
    #[serde(rename = "gate-2")]
    Gate2,
}

/// Immutable ownership assigned before any learned transformation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum StudySplit {
    Train,
    Validation,
    Heldout,
}

/// The protocol condition represented by one truth interval.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum StudyCondition {
    IntentionalTap,
    Quiet,
    ScriptedMotion,
    SharedMotion,
    PhysicalKeyboardTyping,
    DeskTapping,
    SimultaneousAmbiguous,
}

/// Independent reference used to establish contact or class truth.
///
/// Candidate-sensor inference is intentionally not a variant. It cannot be
/// supplied accidentally as its own ground truth.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum IndependentTruthSource {
    ContactSwitch,
    HighSpeedVideo,
    InstrumentedKeyboard,
    OpticalTracker,
}

/// Whether event features are direct recorded values or a declared transform.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ObservationRepresentation {
    Raw,
    /// A frozen transform reduces high-dimensional observations to a feature
    /// vector that fits [`PHYSICAL_STUDY_EVIDENCE_ROW_MAX_BYTES`]. The source
    /// artifact and transform hashes preserve the full observation lineage.
    Derived,
}

/// Exact source and transform provenance for the row's candidate observations.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ObservationSource {
    pub representation: ObservationRepresentation,
    pub artifact_sha256: String,
    /// Required for derived features and forbidden for raw values.
    pub transform_sha256: Option<String>,
    /// Splits used to fit the declared transform. Raw values require an empty
    /// list. Derived values require a nonempty list that excludes `heldout`.
    pub transform_fit_splits: Vec<StudySplit>,
}

/// One event emitted by the candidate contact detector.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateObservation {
    pub event_id: String,
    /// Event time mapped onto the row's declared truth timeline.
    pub event_time_us: u64,
    /// Fixed integer feature vector used by Gates 1 and 2. Gate 0B may leave it
    /// empty because that gate tests only contact observability and timing.
    /// High-dimensional recordings must use a frozen derived representation;
    /// they must not expand this row beyond the declared byte limit.
    pub features: Vec<i32>,
}

/// Prompt, truth, and clock facts for one immutable protocol interval.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PromptTruthClock {
    pub prompt_id: String,
    pub prompt_ordinal: u64,
    pub timeline_id: String,
    pub clock_mapping_sha256: String,
    pub prompt_time_us: u64,
    pub truth_start_us: u64,
    /// Present for an intentional contact and absent for a negative interval.
    pub truth_event_time_us: Option<u64>,
    pub truth_end_us: u64,
}

/// A self-contained row in the physical-study JSONL stream.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PhysicalStudyEvidenceRow {
    pub schema: String,
    /// SHA-256 of this struct's canonical JSON with this field set to `""`.
    pub row_sha256: String,
    pub gate: PhysicalStudyGate,
    pub round_id: String,
    pub split: StudySplit,
    pub split_manifest_sha256: String,
    pub analysis_plan_sha256: String,
    /// Hash of the frozen contact detector that emitted `candidate_events`.
    pub contact_detector_sha256: String,
    /// Declared detector-fit ownership. It may use train and validation, but
    /// never heldout evidence.
    pub contact_detector_fit_splits: Vec<StudySplit>,
    pub participant_id: String,
    pub session_id: String,
    pub rig_id: String,
    pub controller_id: String,
    pub mount_id: String,
    pub surface_id: String,
    pub hand: Hand,
    pub condition: StudyCondition,
    /// `thumb` through `pinky` for Gates 0B/1, an exact 34-slot manifest label
    /// for Gate 2, and `None` for a negative or ambiguous interval.
    pub prompted_label: Option<String>,
    pub truth_label: Option<String>,
    pub truth_source: IndependentTruthSource,
    pub clock: PromptTruthClock,
    pub observation_source: ObservationSource,
    pub candidate_events: Vec<CandidateObservation>,
}

/// One exact expected prompt row in the split manifest. Observation values and
/// artifact hashes are unknown before collection, but every protocol identity,
/// split, class, condition, and ordinal is frozen here.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectedStudyRow {
    pub participant_id: String,
    pub session_id: String,
    pub rig_id: String,
    pub controller_id: String,
    pub mount_id: String,
    pub surface_id: String,
    pub hand: Hand,
    pub split: StudySplit,
    pub condition: StudyCondition,
    pub prompted_label: Option<String>,
    pub truth_source: IndependentTruthSource,
    pub prompt_id: String,
    pub prompt_ordinal: u64,
}

impl From<&PhysicalStudyEvidenceRow> for ExpectedStudyRow {
    fn from(row: &PhysicalStudyEvidenceRow) -> Self {
        Self {
            participant_id: row.participant_id.clone(),
            session_id: row.session_id.clone(),
            rig_id: row.rig_id.clone(),
            controller_id: row.controller_id.clone(),
            mount_id: row.mount_id.clone(),
            surface_id: row.surface_id.clone(),
            hand: row.hand,
            split: row.split,
            condition: row.condition,
            prompted_label: row.prompted_label.clone(),
            truth_source: row.truth_source,
            prompt_id: row.clock.prompt_id.clone(),
            prompt_ordinal: row.clock.prompt_ordinal,
        }
    }
}

/// Exact pre-collection roster. Its bytes, not merely its parsed values, are
/// hashed into every evidence row and into the analysis plan.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PhysicalStudySplitManifest {
    pub schema: String,
    pub gate: PhysicalStudyGate,
    pub round_id: String,
    pub expected_rows: Vec<ExpectedStudyRow>,
}

impl PhysicalStudySplitManifest {
    /// Build a sorted roster. Collection software should call this before it
    /// records observations; tests use it to create real immutable fixtures.
    pub fn from_rows(rows: &[PhysicalStudyEvidenceRow]) -> Result<Self, PhysicalStudyError> {
        let first = rows.first().ok_or_else(|| {
            PhysicalStudyError::MissingStratum("cannot roster an empty study".to_owned())
        })?;
        if rows
            .iter()
            .any(|row| row.gate != first.gate || row.round_id != first.round_id)
        {
            return Err(PhysicalStudyError::MixedStudy(
                "split manifest rows mix gate or round".to_owned(),
            ));
        }
        let mut expected_rows = rows.iter().map(ExpectedStudyRow::from).collect::<Vec<_>>();
        expected_rows.sort();
        if expected_rows.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(PhysicalStudyError::DuplicateEvidence(
                "split manifest roster entry".to_owned(),
            ));
        }
        Ok(Self {
            schema: PHYSICAL_STUDY_SPLIT_MANIFEST_SCHEMA.to_owned(),
            gate: first.gate,
            round_id: first.round_id.clone(),
            expected_rows,
        })
    }

    pub fn canonical_json_bytes(&self) -> Result<Vec<u8>, PhysicalStudyError> {
        bounded_canonical_pretty_json(self, PhysicalStudyArtifact::SplitManifest)
    }
}

/// Frozen analysis settings. The v1 engine accepts only the canonical values
/// returned by [`PhysicalStudyAnalysisPlan::v1`]. A changed threshold or method
/// requires a new schema instead of a post-hoc edit.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FrozenStudyPipeline {
    pub contact_detector_sha256: String,
    pub contact_detector_fit_splits: Vec<StudySplit>,
    pub observation_representation: ObservationRepresentation,
    pub transform_sha256: Option<String>,
    pub transform_fit_splits: Vec<StudySplit>,
}

impl From<&PhysicalStudyEvidenceRow> for FrozenStudyPipeline {
    fn from(row: &PhysicalStudyEvidenceRow) -> Self {
        Self {
            contact_detector_sha256: row.contact_detector_sha256.clone(),
            contact_detector_fit_splits: row.contact_detector_fit_splits.clone(),
            observation_representation: row.observation_source.representation,
            transform_sha256: row.observation_source.transform_sha256.clone(),
            transform_fit_splits: row.observation_source.transform_fit_splits.clone(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PhysicalStudyAnalysisPlan {
    pub schema: String,
    pub gate: PhysicalStudyGate,
    pub round_id: String,
    pub split_manifest_sha256: String,
    pub pipeline: FrozenStudyPipeline,
    pub match_before_us: u64,
    pub match_after_us: u64,
    pub minimum_events_per_class_per_mount: u64,
    pub minimum_negative_exposure_us_per_condition: u64,
    pub minimum_heldout_participants: u64,
    pub minimum_heldout_mounts_per_participant: u64,
    pub require_both_hands: bool,
    pub minimum_contact_latency_us: i64,
    pub contact_latency_p95_limit_us: i64,
    pub contact_latency_p99_limit_us: i64,
    pub miss_upper_limit_ppm: u32,
    pub event_error_upper_limit_ppm: u32,
    pub minimum_class_recall_ppm: u32,
    pub minimum_participant_macro_f1_ppm: u32,
    pub participant_bootstrap_replicates: u64,
    pub one_sided_alpha_ppm: u32,
    pub classifier_algorithm: String,
    pub threshold_selection: String,
}

impl PhysicalStudyAnalysisPlan {
    pub fn v1(
        gate: PhysicalStudyGate,
        round_id: impl Into<String>,
        split_manifest_sha256: impl Into<String>,
        pipeline: FrozenStudyPipeline,
    ) -> Self {
        Self {
            schema: PHYSICAL_STUDY_ANALYSIS_PLAN_SCHEMA.to_owned(),
            gate,
            round_id: round_id.into(),
            split_manifest_sha256: split_manifest_sha256.into(),
            pipeline,
            match_before_us: MATCH_BEFORE_US,
            match_after_us: MATCH_AFTER_US,
            minimum_events_per_class_per_mount: MIN_EVENTS_PER_CLASS_PER_MOUNT,
            minimum_negative_exposure_us_per_condition: match gate {
                PhysicalStudyGate::Gate0B | PhysicalStudyGate::Gate1 => {
                    MIN_NEGATIVE_EXPOSURE_US_PER_CONDITION
                }
                PhysicalStudyGate::Gate2 => 0,
            },
            minimum_heldout_participants: match gate {
                PhysicalStudyGate::Gate0B | PhysicalStudyGate::Gate1 => 5,
                PhysicalStudyGate::Gate2 => 8,
            },
            minimum_heldout_mounts_per_participant: 2,
            require_both_hands: true,
            minimum_contact_latency_us: MIN_CONTACT_LATENCY_US,
            contact_latency_p95_limit_us: LATENCY_P95_LIMIT_US,
            contact_latency_p99_limit_us: LATENCY_P99_LIMIT_US,
            miss_upper_limit_ppm: MISS_UPPER_LIMIT_PPM,
            event_error_upper_limit_ppm: EVENT_ERROR_UPPER_LIMIT_PPM,
            minimum_class_recall_ppm: MIN_RECALL_PPM,
            minimum_participant_macro_f1_ppm: MIN_MACRO_F1_PPM,
            participant_bootstrap_replicates: BOOTSTRAP_REPLICATES as u64,
            one_sided_alpha_ppm: ONE_SIDED_ALPHA_PPM,
            classifier_algorithm: "training-only nearest-centroid squared distance".to_owned(),
            threshold_selection: "validation correct-class distance p95, capped below the nearest negative distance when separable".to_owned(),
        }
    }

    pub fn canonical_json_bytes(&self) -> Result<Vec<u8>, PhysicalStudyError> {
        bounded_canonical_pretty_json(self, PhysicalStudyArtifact::AnalysisPlan)
    }
}

/// Final gate disposition. Inconclusive is never collapsed into failure.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum GateDisposition {
    Go,
    NoGo,
    Inconclusive,
}

/// Outcome of one frozen quantitative criterion.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CriterionOutcome {
    Pass,
    Fail,
    Inconclusive,
}

/// Machine-readable criterion, including the point and confidence boundary.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CriterionReport {
    pub id: String,
    pub comparison: String,
    pub threshold: i64,
    pub unit: String,
    pub point: i64,
    pub confidence_bound: Option<i64>,
    pub confidence_method: Option<String>,
    pub outcome: CriterionOutcome,
}

/// Per-class count metrics. Values use parts per million to avoid ambiguous
/// JSON floating-point formatting.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ClassMetric {
    pub label: String,
    pub support: u64,
    pub true_positive: u64,
    pub false_positive: u64,
    pub false_negative: u64,
    pub precision_ppm: u32,
    pub recall_ppm: u32,
    pub f1_ppm: u32,
}

/// Count confusion matrix. `counts[truth][prediction]` follows `labels`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConfusionMatrix {
    pub labels: Vec<String>,
    pub counts: Vec<Vec<u64>>,
}

/// Metrics shared by the three physical gates.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PhysicalStudyMetrics {
    pub positive_trials: u64,
    pub negative_trials: u64,
    pub negative_exposure_us: u64,
    pub matched_events: u64,
    pub misses: u64,
    pub extra_events: u64,
    pub duplicate_events: u64,
    pub false_activations: u64,
    pub false_activations_milli_per_hour: Option<u64>,
    pub event_errors: u64,
    pub miss_rate_ppm: Option<u32>,
    pub miss_upper_95_ppm: Option<u32>,
    pub event_error_rate_ppm: Option<u32>,
    pub event_error_upper_95_ppm: Option<u32>,
    pub latency_min_us: Option<i64>,
    pub latency_p95_us: Option<i64>,
    pub latency_p95_upper_95_us: Option<i64>,
    pub latency_p99_us: Option<i64>,
    pub latency_p99_upper_95_us: Option<i64>,
    pub participant_minimum_class_recall_ppm: Option<u32>,
    pub participant_minimum_class_recall_lower_95_ppm: Option<u32>,
    pub participant_macro_f1_ppm: Option<u32>,
    pub participant_macro_f1_lower_95_ppm: Option<u32>,
    pub per_class: Vec<ClassMetric>,
    pub confusion_matrix: Option<ConfusionMatrix>,
}

/// Cohort and remount coverage in the scored held-out split.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CohortSummary {
    pub participant_count: u64,
    pub session_count: u64,
    pub mount_count: u64,
    pub rig_count: u64,
    pub controller_count: u64,
    pub surface_count: u64,
}

/// Stable per-participant or per-remount slice.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StudyGroupSummary {
    pub participant_id: String,
    pub mount_id: Option<String>,
    pub surfaces: Vec<String>,
    pub sessions: Vec<String>,
    pub positive_trials: u64,
    pub misses: u64,
    pub event_errors: u64,
    pub macro_f1_ppm: Option<u32>,
}

/// Exact input and lineage hashes needed to reproduce a score.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StudySourceBinding {
    pub exact_jsonl_sha256: String,
    pub canonical_row_set_sha256: String,
    pub train_row_set_sha256: Option<String>,
    pub validation_row_set_sha256: Option<String>,
    pub heldout_row_set_sha256: String,
    pub row_count: u64,
    pub observation_artifact_sha256: Vec<String>,
}

/// Reproducible summary of the training-only centroid model and validation-only
/// abstention threshold. Gate 0B has no classifier and returns `None`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FrozenModelSummary {
    pub schema: String,
    pub algorithm: String,
    pub model_sha256: String,
    pub feature_dimensions: u64,
    pub class_labels: Vec<String>,
    pub train_event_count: u64,
    pub validation_row_count: u64,
    /// Exact IEEE-754 bits of the selected squared-distance threshold.
    pub threshold_squared_distance_bits: u64,
    pub threshold_selection: String,
}

/// Complete deterministic gate report.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PhysicalStudyReport {
    pub schema: String,
    pub evidence_schema: String,
    pub evidence_label: String,
    pub limitation: String,
    pub gate: PhysicalStudyGate,
    pub round_id: String,
    pub split_manifest_sha256: String,
    pub analysis_plan_sha256: String,
    pub contact_detector_sha256: String,
    pub contact_detector_fit_splits: Vec<StudySplit>,
    pub observation_representation: ObservationRepresentation,
    pub transform_sha256: Option<String>,
    pub transform_fit_splits: Vec<StudySplit>,
    pub source: StudySourceBinding,
    pub frozen_model: Option<FrozenModelSummary>,
    pub cohort: CohortSummary,
    pub participant_groups: Vec<StudyGroupSummary>,
    pub remount_groups: Vec<StudyGroupSummary>,
    pub metrics: PhysicalStudyMetrics,
    pub criteria: Vec<CriterionReport>,
    pub disposition: GateDisposition,
}

impl PhysicalStudyReport {
    /// Canonical pretty JSON with one trailing newline.
    pub fn canonical_json_bytes(&self) -> Result<Vec<u8>, PhysicalStudyError> {
        bounded_canonical_pretty_json(self, PhysicalStudyArtifact::Report)
    }
}

fn bounded_canonical_pretty_json(
    value: &impl Serialize,
    artifact: PhysicalStudyArtifact,
) -> Result<Vec<u8>, PhysicalStudyError> {
    let mut writer = BoundedJsonWriter::new(artifact.maximum_bytes());
    serde_json::to_writer_pretty(&mut writer, value)
        .map_err(|error| PhysicalStudyError::Serialization(error.to_string()))?;
    writer
        .write_all(b"\n")
        .map_err(|error| PhysicalStudyError::Serialization(error.to_string()))?;
    writer.into_artifact_bytes(artifact)
}

/// Retains only a bounded prefix while letting Serde finish, so the typed
/// error can report the exact encoded size without an oversized allocation.
struct BoundedJsonWriter {
    bytes: Vec<u8>,
    actual_bytes: usize,
    maximum_retained_bytes: usize,
}

impl BoundedJsonWriter {
    fn new(maximum_retained_bytes: usize) -> Self {
        Self {
            bytes: Vec::new(),
            actual_bytes: 0,
            maximum_retained_bytes,
        }
    }

    fn into_artifact_bytes(
        self,
        artifact: PhysicalStudyArtifact,
    ) -> Result<Vec<u8>, PhysicalStudyError> {
        validate_physical_study_artifact_size(artifact, self.actual_bytes)?;
        Ok(self.bytes)
    }

    fn into_row_bytes(self, line: usize) -> Result<Vec<u8>, PhysicalStudyError> {
        require_evidence_row_size(self.actual_bytes, line)?;
        Ok(self.bytes)
    }
}

impl Write for BoundedJsonWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.actual_bytes = self
            .actual_bytes
            .checked_add(buffer.len())
            .ok_or_else(|| io::Error::other("encoded JSON size overflow"))?;
        let retained = buffer
            .len()
            .min(self.maximum_retained_bytes.saturating_sub(self.bytes.len()));
        self.bytes.extend_from_slice(&buffer[..retained]);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Bounded JSON artifact kinds accepted by this module's public decoders.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PhysicalStudyArtifact {
    EvidenceJsonl,
    SplitManifest,
    AnalysisPlan,
    Report,
}

impl PhysicalStudyArtifact {
    fn name(self) -> &'static str {
        match self {
            Self::EvidenceJsonl => "evidence JSONL",
            Self::SplitManifest => "split manifest JSON",
            Self::AnalysisPlan => "analysis plan JSON",
            Self::Report => "report JSON",
        }
    }

    fn maximum_bytes(self) -> usize {
        match self {
            Self::EvidenceJsonl => PHYSICAL_STUDY_EVIDENCE_JSONL_MAX_BYTES,
            Self::SplitManifest => PHYSICAL_STUDY_SPLIT_MANIFEST_MAX_BYTES,
            Self::AnalysisPlan => PHYSICAL_STUDY_ANALYSIS_PLAN_MAX_BYTES,
            Self::Report => PHYSICAL_STUDY_REPORT_MAX_BYTES,
        }
    }

    fn invalid_json_label(self) -> &'static str {
        match self {
            Self::EvidenceJsonl => "evidence JSONL",
            Self::SplitManifest => "split manifest",
            Self::AnalysisPlan => "analysis plan",
            Self::Report => "physical-study report",
        }
    }
}

/// Fail-closed validation or scoring error.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PhysicalStudyError {
    InvalidJson {
        line: usize,
        message: String,
    },
    ArtifactTooLarge {
        artifact: PhysicalStudyArtifact,
        actual_bytes: usize,
        maximum_bytes: usize,
    },
    EvidenceRowTooLarge {
        line: usize,
        actual_bytes: usize,
        maximum_bytes: usize,
    },
    InvalidField {
        row_id: String,
        message: String,
    },
    RowHashMismatch {
        row_id: String,
    },
    DuplicateEvidence(String),
    MixedStudy(String),
    Leakage(String),
    MissingStratum(String),
    Arithmetic(String),
    Serialization(String),
}

impl fmt::Display for PhysicalStudyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidJson { line, message } => {
                write!(formatter, "invalid evidence JSON on line {line}: {message}")
            }
            Self::ArtifactTooLarge {
                artifact,
                actual_bytes,
                maximum_bytes,
            } => write!(
                formatter,
                "{} is {actual_bytes} bytes; maximum is {maximum_bytes} bytes",
                artifact.name()
            ),
            Self::EvidenceRowTooLarge {
                line,
                actual_bytes,
                maximum_bytes,
            } => write!(
                formatter,
                "evidence row {line} is {actual_bytes} bytes; maximum is {maximum_bytes} bytes"
            ),
            Self::InvalidField { row_id, message } => {
                write!(formatter, "invalid evidence row {row_id}: {message}")
            }
            Self::RowHashMismatch { row_id } => {
                write!(formatter, "evidence row hash mismatch: {row_id}")
            }
            Self::DuplicateEvidence(message) => write!(formatter, "duplicate evidence: {message}"),
            Self::MixedStudy(message) => write!(formatter, "mixed study input: {message}"),
            Self::Leakage(message) => write!(formatter, "split leakage: {message}"),
            Self::MissingStratum(message) => write!(formatter, "missing stratum: {message}"),
            Self::Arithmetic(message) => write!(formatter, "study arithmetic failed: {message}"),
            Self::Serialization(message) => write!(formatter, "serialization failed: {message}"),
        }
    }
}

impl Error for PhysicalStudyError {}

/// Compute the semantic row hash used by [`seal_physical_study_evidence_row`].
pub fn physical_study_row_sha256(
    row: &PhysicalStudyEvidenceRow,
) -> Result<String, PhysicalStudyError> {
    row_hash_and_payload_size(row).map(|(hash, _)| hash)
}

fn row_hash_and_payload_size(
    row: &PhysicalStudyEvidenceRow,
) -> Result<(String, usize), PhysicalStudyError> {
    let mut payload = row.clone();
    payload.row_sha256.clear();
    let bytes = canonical_evidence_row_bytes(&payload, 1)?;
    Ok((format_sha256(&sha256(&bytes)), bytes.len()))
}

/// Seal a newly collected row after all other fields are final.
pub fn seal_physical_study_evidence_row(
    row: &mut PhysicalStudyEvidenceRow,
) -> Result<(), PhysicalStudyError> {
    let (hash, payload_size) = row_hash_and_payload_size(row)?;
    require_evidence_row_size(payload_size + hash.len(), 1)?;
    row.row_sha256 = hash;
    Ok(())
}

/// Encode already-sealed rows as canonical JSONL.
pub fn encode_physical_study_jsonl(
    rows: &[PhysicalStudyEvidenceRow],
) -> Result<Vec<u8>, PhysicalStudyError> {
    let mut writer = BoundedJsonWriter::new(PHYSICAL_STUDY_EVIDENCE_JSONL_MAX_BYTES);
    for (index, row) in rows.iter().enumerate() {
        let row_bytes = canonical_evidence_row_bytes(row, index + 1)?;
        writer
            .write_all(&row_bytes)
            .and_then(|_| writer.write_all(b"\n"))
            .map_err(|error| PhysicalStudyError::Serialization(error.to_string()))?;
    }
    writer.into_artifact_bytes(PhysicalStudyArtifact::EvidenceJsonl)
}

fn canonical_evidence_row_bytes(
    row: &PhysicalStudyEvidenceRow,
    line: usize,
) -> Result<Vec<u8>, PhysicalStudyError> {
    let mut writer = BoundedJsonWriter::new(PHYSICAL_STUDY_EVIDENCE_ROW_MAX_BYTES);
    serde_json::to_writer(&mut writer, row)
        .map_err(|error| PhysicalStudyError::Serialization(error.to_string()))?;
    writer.into_row_bytes(line)
}

/// Parse bounded evidence or roster-template JSONL without applying semantic
/// validation. Planning may use unsealed rows; scoring validates every decoded
/// row, including its hash, before analysis.
pub fn decode_physical_study_jsonl(
    source: &[u8],
) -> Result<Vec<PhysicalStudyEvidenceRow>, PhysicalStudyError> {
    validate_physical_study_artifact_size(PhysicalStudyArtifact::EvidenceJsonl, source.len())?;
    if source.is_empty() {
        return Err(PhysicalStudyError::MissingStratum(
            "the evidence JSONL is empty".to_owned(),
        ));
    }

    let mut rows = Vec::new();
    let mut lines = source.split(|byte| *byte == b'\n').peekable();
    let mut line_number = 0_usize;
    while let Some(line) = lines.next() {
        line_number += 1;
        if line.is_empty() && lines.peek().is_none() {
            continue;
        }
        require_evidence_row_size(line.len(), line_number)?;
        if line.iter().all(u8::is_ascii_whitespace) {
            return Err(PhysicalStudyError::InvalidJson {
                line: line_number,
                message: "blank JSONL rows are forbidden".to_owned(),
            });
        }
        rows.push(parse_evidence_row(line, line_number)?);
    }
    if rows.is_empty() {
        return Err(PhysicalStudyError::MissingStratum(
            "the evidence JSONL has no rows".to_owned(),
        ));
    }
    Ok(rows)
}

/// Parse one bounded evidence-row JSON document. The document may be pretty
/// printed and unsealed so collection software can fill and seal it afterward.
pub fn decode_physical_study_evidence_row(
    source: &[u8],
) -> Result<PhysicalStudyEvidenceRow, PhysicalStudyError> {
    require_evidence_row_size(source.len(), 1)?;
    parse_evidence_row(source, 1)
}

fn require_evidence_row_size(actual_bytes: usize, line: usize) -> Result<(), PhysicalStudyError> {
    if actual_bytes > PHYSICAL_STUDY_EVIDENCE_ROW_MAX_BYTES {
        return Err(PhysicalStudyError::EvidenceRowTooLarge {
            line,
            actual_bytes,
            maximum_bytes: PHYSICAL_STUDY_EVIDENCE_ROW_MAX_BYTES,
        });
    }
    Ok(())
}

fn parse_evidence_row(
    source: &[u8],
    line: usize,
) -> Result<PhysicalStudyEvidenceRow, PhysicalStudyError> {
    serde_json::from_slice(source).map_err(|error| PhysicalStudyError::InvalidJson {
        line,
        message: error.to_string(),
    })
}

/// Decode a bounded exact split-manifest JSON artifact.
pub fn decode_physical_study_split_manifest(
    source: &[u8],
) -> Result<PhysicalStudySplitManifest, PhysicalStudyError> {
    decode_bounded_json(source, PhysicalStudyArtifact::SplitManifest)
}

/// Decode a bounded exact analysis-plan JSON artifact.
pub fn decode_physical_study_analysis_plan(
    source: &[u8],
) -> Result<PhysicalStudyAnalysisPlan, PhysicalStudyError> {
    decode_bounded_json(source, PhysicalStudyArtifact::AnalysisPlan)
}

/// Decode a bounded canonical physical-study report JSON artifact.
pub fn decode_physical_study_report(
    source: &[u8],
) -> Result<PhysicalStudyReport, PhysicalStudyError> {
    decode_bounded_json(source, PhysicalStudyArtifact::Report)
}

fn decode_bounded_json<T: DeserializeOwned>(
    source: &[u8],
    artifact: PhysicalStudyArtifact,
) -> Result<T, PhysicalStudyError> {
    validate_physical_study_artifact_size(artifact, source.len())?;
    serde_json::from_slice(source).map_err(|error| PhysicalStudyError::InvalidJson {
        line: 0,
        message: format!("invalid {}: {error}", artifact.invalid_json_label()),
    })
}

/// Validate a prospective exact artifact length against the same limit used by
/// the bounded encoders and decoders. Append-only collectors should call this
/// while holding their file lock, before writing the next canonical row.
pub fn validate_physical_study_artifact_size(
    artifact: PhysicalStudyArtifact,
    actual_bytes: usize,
) -> Result<(), PhysicalStudyError> {
    let maximum_bytes = artifact.maximum_bytes();
    if actual_bytes > maximum_bytes {
        return Err(PhysicalStudyError::ArtifactTooLarge {
            artifact,
            actual_bytes,
            maximum_bytes,
        });
    }
    Ok(())
}

/// Validate, fit, and score an exact JSONL evidence artifact.
///
/// Gates 1 and 2 fit centroids only from `train`, select one threshold only
/// from `validation`, and make the gate decision only from `heldout`.
pub fn analyze_physical_study_jsonl(
    source: &[u8],
    split_manifest_source: &[u8],
    analysis_plan_source: &[u8],
) -> Result<PhysicalStudyReport, PhysicalStudyError> {
    let rows = decode_physical_study_jsonl(source)?;
    for row in &rows {
        validate_row(row)?;
    }

    validate_frozen_artifacts(&rows, split_manifest_source, analysis_plan_source)?;
    let exact_source_sha256 = format_sha256(&sha256(source));
    analyze_rows(&rows, exact_source_sha256)
}

fn validate_frozen_artifacts(
    rows: &[PhysicalStudyEvidenceRow],
    split_manifest_source: &[u8],
    analysis_plan_source: &[u8],
) -> Result<(), PhysicalStudyError> {
    let manifest = decode_physical_study_split_manifest(split_manifest_source)?;
    let first = &rows[0];
    if manifest.schema != PHYSICAL_STUDY_SPLIT_MANIFEST_SCHEMA
        || manifest.gate != first.gate
        || manifest.round_id != first.round_id
    {
        return Err(PhysicalStudyError::MixedStudy(
            "split manifest schema, gate, or round does not match evidence".to_owned(),
        ));
    }
    let manifest_sha256 = format_sha256(&sha256(split_manifest_source));
    if manifest_sha256 != first.split_manifest_sha256 {
        return Err(PhysicalStudyError::Leakage(
            "evidence does not bind the supplied split manifest bytes".to_owned(),
        ));
    }
    let mut expected_rows = manifest.expected_rows;
    expected_rows.sort();
    if expected_rows.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(PhysicalStudyError::DuplicateEvidence(
            "split manifest roster entry".to_owned(),
        ));
    }
    let mut actual_rows = rows.iter().map(ExpectedStudyRow::from).collect::<Vec<_>>();
    actual_rows.sort();
    if actual_rows != expected_rows {
        return Err(PhysicalStudyError::Leakage(
            "evidence rows do not exactly equal the frozen split-manifest roster".to_owned(),
        ));
    }

    let plan = decode_physical_study_analysis_plan(analysis_plan_source)?;
    let canonical_plan = PhysicalStudyAnalysisPlan::v1(
        first.gate,
        first.round_id.clone(),
        manifest_sha256,
        FrozenStudyPipeline::from(first),
    );
    if plan != canonical_plan {
        return Err(PhysicalStudyError::Leakage(
            "analysis plan differs from the frozen v1 gate contract".to_owned(),
        ));
    }
    let plan_sha256 = format_sha256(&sha256(analysis_plan_source));
    if plan_sha256 != first.analysis_plan_sha256 {
        return Err(PhysicalStudyError::Leakage(
            "evidence does not bind the supplied analysis plan bytes".to_owned(),
        ));
    }
    Ok(())
}

fn validate_row(row: &PhysicalStudyEvidenceRow) -> Result<(), PhysicalStudyError> {
    let row_id = row.clock.prompt_id.clone();
    if row.schema != PHYSICAL_STUDY_EVIDENCE_SCHEMA {
        return invalid(&row_id, "unsupported evidence schema");
    }
    for (name, value) in [
        ("round_id", row.round_id.as_str()),
        ("participant_id", row.participant_id.as_str()),
        ("session_id", row.session_id.as_str()),
        ("rig_id", row.rig_id.as_str()),
        ("controller_id", row.controller_id.as_str()),
        ("mount_id", row.mount_id.as_str()),
        ("surface_id", row.surface_id.as_str()),
        ("prompt_id", row.clock.prompt_id.as_str()),
        ("timeline_id", row.clock.timeline_id.as_str()),
    ] {
        if value.is_empty() || value.len() > 128 || value.chars().any(char::is_control) {
            return invalid(
                &row_id,
                &format!("{name} is empty, too long, or contains controls"),
            );
        }
    }
    for (name, value) in [
        ("row_sha256", row.row_sha256.as_str()),
        ("split_manifest_sha256", row.split_manifest_sha256.as_str()),
        ("analysis_plan_sha256", row.analysis_plan_sha256.as_str()),
        (
            "contact_detector_sha256",
            row.contact_detector_sha256.as_str(),
        ),
        (
            "clock_mapping_sha256",
            row.clock.clock_mapping_sha256.as_str(),
        ),
        (
            "observation artifact_sha256",
            row.observation_source.artifact_sha256.as_str(),
        ),
    ] {
        require_canonical_hash(&row_id, name, value)?;
    }
    validate_fit_splits(
        &row.clock.prompt_id,
        "contact detector",
        &row.contact_detector_fit_splits,
    )?;
    match (
        row.observation_source.representation,
        row.observation_source.transform_sha256.as_deref(),
        row.observation_source.transform_fit_splits.as_slice(),
    ) {
        (ObservationRepresentation::Raw, None, []) => {}
        (ObservationRepresentation::Derived, Some(hash), fit_splits) => {
            require_canonical_hash(&row_id, "transform_sha256", hash)?;
            validate_fit_splits(&row.clock.prompt_id, "observation transform", fit_splits)?;
        }
        (ObservationRepresentation::Raw, _, _) => {
            return invalid(
                &row_id,
                "raw observations cannot declare a transform or transform-fit split",
            );
        }
        (ObservationRepresentation::Derived, None, _) => {
            return invalid(
                &row_id,
                "derived observations require transform_sha256 and train/validation fit ownership",
            );
        }
    }
    if physical_study_row_sha256(row)? != row.row_sha256 {
        return Err(PhysicalStudyError::RowHashMismatch { row_id });
    }
    if row.clock.prompt_time_us > row.clock.truth_start_us
        || row.clock.truth_start_us > row.clock.truth_end_us
    {
        return invalid(
            &row.clock.prompt_id,
            "prompt and truth interval are not monotonic",
        );
    }
    match (row.condition, row.clock.truth_event_time_us) {
        (StudyCondition::IntentionalTap, Some(event_time))
            if event_time
                .checked_sub(MATCH_BEFORE_US)
                .is_some_and(|start| row.clock.truth_start_us <= start)
                && event_time
                    .checked_add(MATCH_AFTER_US)
                    .is_some_and(|end| row.clock.truth_end_us >= end) => {}
        (StudyCondition::IntentionalTap, _) => {
            return invalid(
                &row.clock.prompt_id,
                "intentional taps require a truth event with complete 50,000us before and 250,000us after",
            );
        }
        (_, None) if row.clock.truth_start_us < row.clock.truth_end_us => {}
        (_, _) => {
            return invalid(
                &row.clock.prompt_id,
                "negative intervals require no truth event and positive duration",
            );
        }
    }
    if row.condition == StudyCondition::IntentionalTap {
        if row.prompted_label.is_none() || row.prompted_label != row.truth_label {
            return invalid(
                &row.clock.prompt_id,
                "intentional prompt and independent truth labels must match",
            );
        }
    } else if row.prompted_label.is_some() || row.truth_label.is_some() {
        return invalid(
            &row.clock.prompt_id,
            "negative and ambiguous intervals cannot carry a class label",
        );
    }
    validate_gate_label(row)?;

    let mut previous_event_time = None;
    for event in &row.candidate_events {
        if event.event_id.is_empty()
            || event.event_id.len() > 128
            || event.event_id.chars().any(char::is_control)
        {
            return invalid(&row.clock.prompt_id, "invalid candidate event_id");
        }
        if event.event_time_us < row.clock.truth_start_us
            || event.event_time_us > row.clock.truth_end_us
        {
            return invalid(
                &row.clock.prompt_id,
                "candidate event falls outside its truth interval",
            );
        }
        if previous_event_time.is_some_and(|previous| event.event_time_us <= previous) {
            return invalid(
                &row.clock.prompt_id,
                "candidate event times must be strictly increasing",
            );
        }
        if row.gate != PhysicalStudyGate::Gate0B && event.features.is_empty() {
            return invalid(
                &row.clock.prompt_id,
                "Gate 1/2 candidate events require features",
            );
        }
        if event.features.len() > 4_096 {
            return invalid(&row.clock.prompt_id, "feature vector exceeds 4096 values");
        }
        previous_event_time = Some(event.event_time_us);
    }
    Ok(())
}

fn validate_fit_splits(
    row_id: &str,
    owner: &str,
    splits: &[StudySplit],
) -> Result<(), PhysicalStudyError> {
    if splits.is_empty()
        || splits.windows(2).any(|pair| pair[0] >= pair[1])
        || splits.contains(&StudySplit::Heldout)
    {
        return invalid(
            row_id,
            &format!(
                "{owner} fit splits must be a nonempty sorted unique subset of train/validation"
            ),
        );
    }
    Ok(())
}

fn validate_gate_label(row: &PhysicalStudyEvidenceRow) -> Result<(), PhysicalStudyError> {
    let Some(label) = row.truth_label.as_deref() else {
        return Ok(());
    };
    match row.gate {
        PhysicalStudyGate::Gate0B | PhysicalStudyGate::Gate1 => {
            if !matches!(label, "thumb" | "index" | "middle" | "ring" | "pinky") {
                return invalid(&row.clock.prompt_id, "unknown finger truth label");
            }
        }
        PhysicalStudyGate::Gate2 => {
            let slot = slot_for_label(label).ok_or_else(|| PhysicalStudyError::InvalidField {
                row_id: row.clock.prompt_id.clone(),
                message: "unknown Core-28 physical position label".to_owned(),
            })?;
            if slot.position.hand != row.hand {
                return invalid(
                    &row.clock.prompt_id,
                    "truth label belongs to the other hand",
                );
            }
        }
    }
    Ok(())
}

fn require_canonical_hash(row_id: &str, name: &str, value: &str) -> Result<(), PhysicalStudyError> {
    let Some(hash) = parse_sha256(value) else {
        return invalid(row_id, &format!("{name} is not SHA-256"));
    };
    if format_sha256(&hash) != value {
        return invalid(row_id, &format!("{name} is not canonical lowercase hex"));
    }
    Ok(())
}

fn invalid<T>(row_id: &str, message: &str) -> Result<T, PhysicalStudyError> {
    Err(PhysicalStudyError::InvalidField {
        row_id: row_id.to_owned(),
        message: message.to_owned(),
    })
}

#[derive(Clone, Debug, Serialize)]
struct CentroidModelArtifact {
    schema: &'static str,
    gate: PhysicalStudyGate,
    feature_dimensions: usize,
    classes: Vec<CentroidClass>,
    threshold_squared_distance_bits: u64,
}

#[derive(Clone, Debug, Serialize)]
struct CentroidClass {
    label: String,
    count: u64,
    sums: Vec<i64>,
}

#[derive(Clone, Debug)]
struct CentroidModel {
    artifact: CentroidModelArtifact,
    /// Runtime-only coordinates. The serialized artifact retains integer sums
    /// and counts so caching cannot change its reproducible hash.
    centroids: Vec<Vec<f64>>,
}

impl CentroidModel {
    fn new(artifact: CentroidModelArtifact) -> Self {
        let centroids = artifact
            .classes
            .iter()
            .map(|class| {
                let count = class.count as f64;
                class.sums.iter().map(|sum| *sum as f64 / count).collect()
            })
            .collect();
        Self {
            artifact,
            centroids,
        }
    }

    fn distance(&self, class_index: usize, features: &[i32]) -> f64 {
        self.centroids[class_index]
            .iter()
            .zip(features)
            .map(|(centroid, value)| {
                let delta = f64::from(*value) - centroid;
                delta * delta
            })
            .sum()
    }

    fn nearest(&self, features: &[i32]) -> (usize, f64) {
        self.artifact
            .classes
            .iter()
            .enumerate()
            .map(|(index, _)| (index, self.distance(index, features)))
            .min_by(|(left_index, left), (right_index, right)| {
                left.total_cmp(right).then(left_index.cmp(right_index))
            })
            .expect("validated models have at least one class")
    }

    fn threshold(&self) -> f64 {
        f64::from_bits(self.artifact.threshold_squared_distance_bits)
    }

    fn classify<'a>(&'a self, features: &[i32]) -> Option<&'a str> {
        let (index, distance) = self.nearest(features);
        (distance <= self.threshold()).then_some(self.artifact.classes[index].label.as_str())
    }
}

fn analyze_rows(
    rows: &[PhysicalStudyEvidenceRow],
    exact_source_sha256: String,
) -> Result<PhysicalStudyReport, PhysicalStudyError> {
    validate_study_identity(rows)?;
    validate_no_leakage(rows)?;
    validate_session_order(rows)?;
    validate_protocol_coverage(rows)?;

    let first = &rows[0];
    let class_labels = physical_class_labels(first.gate);
    let (model, model_summary) = if first.gate == PhysicalStudyGate::Gate0B {
        (None, None)
    } else {
        let mut model = fit_centroids(rows, first.gate, &class_labels)?;
        let threshold = select_validation_threshold(rows, &model)?;
        model.artifact.threshold_squared_distance_bits = threshold.to_bits();
        let artifact_bytes = serde_json::to_vec(&model.artifact)
            .map_err(|error| PhysicalStudyError::Serialization(error.to_string()))?;
        let summary = FrozenModelSummary {
            schema: CENTROID_MODEL_SCHEMA.to_owned(),
            algorithm: "training-only nearest-centroid squared distance".to_owned(),
            model_sha256: format_sha256(&sha256(&artifact_bytes)),
            feature_dimensions: u64::try_from(model.artifact.feature_dimensions)
                .map_err(|_| PhysicalStudyError::Arithmetic("feature dimension does not fit u64".to_owned()))?,
            class_labels: class_labels.clone(),
            train_event_count: model.artifact.classes.iter().map(|class| class.count).sum(),
            validation_row_count: count_rows(rows, StudySplit::Validation)?,
            threshold_squared_distance_bits: threshold.to_bits(),
            threshold_selection: "validation correct-class distance p95, capped below the nearest negative distance when separable; heldout is never inspected".to_owned(),
        };
        (Some(model), Some(summary))
    };

    let heldout = rows
        .iter()
        .filter(|row| row.split == StudySplit::Heldout)
        .collect::<Vec<_>>();
    let classification_cache = model
        .as_ref()
        .map(|model| build_classification_cache(&heldout, first.gate, model, &class_labels));
    let scored = score_rows(
        &heldout,
        first.gate,
        &class_labels,
        classification_cache.as_ref(),
    )?;
    let metrics = build_metrics(
        &heldout,
        first.gate,
        &class_labels,
        classification_cache.as_ref(),
        &scored,
    )?;
    let criteria = build_criteria(first.gate, &metrics);
    let disposition = disposition(&criteria);
    let (participant_groups, remount_groups) = build_groups(
        &heldout,
        first.gate,
        &class_labels,
        classification_cache.as_ref(),
    )?;

    Ok(PhysicalStudyReport {
        schema: PHYSICAL_STUDY_REPORT_SCHEMA.to_owned(),
        evidence_schema: PHYSICAL_STUDY_EVIDENCE_SCHEMA.to_owned(),
        evidence_label: "hardware-dependent-observations; deterministic-analysis".to_owned(),
        limitation: "This report validates provenance, split discipline, scoring, and the declared physical observations. It cannot prove that the rig, truth source, mount, or participant protocol was executed as declared.".to_owned(),
        gate: first.gate,
        round_id: first.round_id.clone(),
        split_manifest_sha256: first.split_manifest_sha256.clone(),
        analysis_plan_sha256: first.analysis_plan_sha256.clone(),
        contact_detector_sha256: first.contact_detector_sha256.clone(),
        contact_detector_fit_splits: first.contact_detector_fit_splits.clone(),
        observation_representation: first.observation_source.representation,
        transform_sha256: first.observation_source.transform_sha256.clone(),
        transform_fit_splits: first
            .observation_source
            .transform_fit_splits
            .clone(),
        source: build_source_binding(rows, exact_source_sha256)?,
        frozen_model: model_summary,
        cohort: cohort_summary(&heldout)?,
        participant_groups,
        remount_groups,
        metrics,
        criteria,
        disposition,
    })
}

fn validate_study_identity(rows: &[PhysicalStudyEvidenceRow]) -> Result<(), PhysicalStudyError> {
    let first = &rows[0];
    for row in rows.iter().skip(1) {
        if row.gate != first.gate
            || row.round_id != first.round_id
            || row.split_manifest_sha256 != first.split_manifest_sha256
            || row.analysis_plan_sha256 != first.analysis_plan_sha256
            || row.contact_detector_sha256 != first.contact_detector_sha256
            || row.contact_detector_fit_splits != first.contact_detector_fit_splits
            || row.observation_source.representation != first.observation_source.representation
            || row.observation_source.transform_sha256 != first.observation_source.transform_sha256
            || row.observation_source.transform_fit_splits
                != first.observation_source.transform_fit_splits
        {
            return Err(PhysicalStudyError::MixedStudy(
                "gate, round, plan, detector, split manifest, representation, and transform must be identical"
                    .to_owned(),
            ));
        }
    }

    let mut dimensions = None;
    if first.gate != PhysicalStudyGate::Gate0B {
        for event in rows.iter().flat_map(|row| &row.candidate_events) {
            if let Some(expected) = dimensions {
                if event.features.len() != expected {
                    return Err(PhysicalStudyError::MixedStudy(
                        "candidate feature dimensions differ".to_owned(),
                    ));
                }
            } else {
                dimensions = Some(event.features.len());
            }
        }
        if dimensions.is_none() {
            return Err(PhysicalStudyError::MissingStratum(
                "Gate 1/2 evidence has no candidate feature vector".to_owned(),
            ));
        }
    }
    Ok(())
}

fn validate_no_leakage(rows: &[PhysicalStudyEvidenceRow]) -> Result<(), PhysicalStudyError> {
    let mut row_ids = BTreeSet::new();
    let mut row_hashes = BTreeSet::new();
    let mut event_ids = BTreeSet::new();
    let mut session_splits = BTreeMap::new();
    let mut mount_splits = BTreeMap::new();
    let mut participant_splits = BTreeMap::new();
    let mut artifact_splits = BTreeMap::new();
    let mut session_owner = BTreeMap::new();
    let mut controller_hand = BTreeMap::new();

    for row in rows {
        let prompt_key = (row.session_id.as_str(), row.clock.prompt_id.as_str());
        if !row_ids.insert(prompt_key) {
            return Err(PhysicalStudyError::DuplicateEvidence(format!(
                "prompt {} in session {}",
                row.clock.prompt_id, row.session_id
            )));
        }
        if !row_hashes.insert(row.row_sha256.as_str()) {
            return Err(PhysicalStudyError::DuplicateEvidence(format!(
                "row hash {}",
                row.row_sha256
            )));
        }
        for event in &row.candidate_events {
            if !event_ids.insert(event.event_id.as_str()) {
                return Err(PhysicalStudyError::DuplicateEvidence(format!(
                    "candidate event {}",
                    event.event_id
                )));
            }
        }
        insert_split(
            &mut session_splits,
            row.session_id.as_str(),
            row.split,
            "session",
        )?;
        insert_split(
            &mut participant_splits,
            row.participant_id.as_str(),
            row.split,
            "participant",
        )?;
        insert_split(
            &mut mount_splits,
            (row.participant_id.as_str(), row.mount_id.as_str()),
            row.split,
            "participant mount",
        )?;
        insert_split(
            &mut artifact_splits,
            row.observation_source.artifact_sha256.as_str(),
            row.split,
            "observation artifact",
        )?;

        let owner = (
            row.participant_id.as_str(),
            row.mount_id.as_str(),
            row.rig_id.as_str(),
            row.surface_id.as_str(),
            row.clock.timeline_id.as_str(),
        );
        if let Some(existing) = session_owner.insert(row.session_id.as_str(), owner)
            && existing != owner
        {
            return Err(PhysicalStudyError::Leakage(format!(
                "session {} changes participant, mount, rig, surface, or timeline",
                row.session_id
            )));
        }
        let controller_key = (row.session_id.as_str(), row.hand);
        if let Some(existing) = controller_hand.insert(controller_key, row.controller_id.as_str())
            && existing != row.controller_id
        {
            return Err(PhysicalStudyError::Leakage(format!(
                "session {} changes the {:?} controller",
                row.session_id, row.hand
            )));
        }
    }
    Ok(())
}

fn insert_split<K: Ord>(
    owners: &mut BTreeMap<K, StudySplit>,
    key: K,
    split: StudySplit,
    name: &str,
) -> Result<(), PhysicalStudyError> {
    if let Some(existing) = owners.insert(key, split)
        && existing != split
    {
        return Err(PhysicalStudyError::Leakage(format!(
            "{name} crosses {existing:?} and {split:?}"
        )));
    }
    Ok(())
}

fn validate_session_order(rows: &[PhysicalStudyEvidenceRow]) -> Result<(), PhysicalStudyError> {
    let mut sessions: BTreeMap<&str, Vec<&PhysicalStudyEvidenceRow>> = BTreeMap::new();
    for row in rows {
        sessions.entry(&row.session_id).or_default().push(row);
    }
    for (session, mut session_rows) in sessions {
        session_rows.sort_by_key(|row| row.clock.prompt_ordinal);
        let mut previous_end = None;
        let mut previous_prompt_time = None;
        for (expected_ordinal, row) in session_rows.into_iter().enumerate() {
            let expected_ordinal = u64::try_from(expected_ordinal).map_err(|_| {
                PhysicalStudyError::Arithmetic("prompt ordinal does not fit u64".to_owned())
            })?;
            if row.clock.prompt_ordinal != expected_ordinal {
                return Err(PhysicalStudyError::Leakage(format!(
                    "session {session} prompt ordinals are not complete from zero"
                )));
            }
            if previous_prompt_time.is_some_and(|previous| row.clock.prompt_time_us <= previous) {
                return Err(PhysicalStudyError::Leakage(format!(
                    "session {session} prompt time is not strictly increasing"
                )));
            }
            if previous_end.is_some_and(|end| row.clock.truth_start_us < end) {
                return Err(PhysicalStudyError::Leakage(format!(
                    "session {session} truth intervals overlap"
                )));
            }
            previous_end = Some(row.clock.truth_end_us);
            previous_prompt_time = Some(row.clock.prompt_time_us);
        }
    }
    Ok(())
}

fn validate_protocol_coverage(rows: &[PhysicalStudyEvidenceRow]) -> Result<(), PhysicalStudyError> {
    let gate = rows[0].gate;
    let heldout = rows
        .iter()
        .filter(|row| row.split == StudySplit::Heldout)
        .collect::<Vec<_>>();
    if heldout.is_empty() {
        return Err(PhysicalStudyError::MissingStratum(
            "heldout split is empty".to_owned(),
        ));
    }
    if gate != PhysicalStudyGate::Gate0B {
        for split in [StudySplit::Train, StudySplit::Validation] {
            if !rows.iter().any(|row| row.split == split) {
                return Err(PhysicalStudyError::MissingStratum(format!(
                    "{split:?} split is empty"
                )));
            }
            for label in physical_class_labels(gate) {
                if !rows.iter().any(|row| {
                    row.split == split && row.truth_label.as_deref() == Some(label.as_str())
                }) {
                    return Err(PhysicalStudyError::MissingStratum(format!(
                        "{split:?} has no {label} truth class"
                    )));
                }
            }
        }
    }

    let minimum_participants = match gate {
        PhysicalStudyGate::Gate0B | PhysicalStudyGate::Gate1 => 5,
        PhysicalStudyGate::Gate2 => 8,
    };
    let participants = heldout
        .iter()
        .map(|row| row.participant_id.as_str())
        .collect::<BTreeSet<_>>();
    if participants.len() < minimum_participants {
        return Err(PhysicalStudyError::MissingStratum(format!(
            "{gate:?} needs at least {minimum_participants} heldout participants"
        )));
    }

    for participant in participants {
        let mounts = heldout
            .iter()
            .filter(|row| row.participant_id == participant)
            .map(|row| row.mount_id.as_str())
            .collect::<BTreeSet<_>>();
        if mounts.len() < 2 {
            return Err(PhysicalStudyError::MissingStratum(format!(
                "participant {participant} needs two heldout mounts"
            )));
        }
        for mount in mounts {
            if matches!(gate, PhysicalStudyGate::Gate0B | PhysicalStudyGate::Gate1) {
                for hand in [Hand::Left, Hand::Right] {
                    let group = heldout
                        .iter()
                        .copied()
                        .filter(|row| {
                            row.participant_id == participant
                                && row.mount_id == mount
                                && row.hand == hand
                        })
                        .collect::<Vec<_>>();
                    validate_mount_coverage(
                        gate,
                        participant,
                        &format!("{mount}/{hand:?}"),
                        &group,
                    )?;
                }
            } else {
                let group = heldout
                    .iter()
                    .copied()
                    .filter(|row| row.participant_id == participant && row.mount_id == mount)
                    .collect::<Vec<_>>();
                validate_mount_coverage(gate, participant, mount, &group)?;
            }
        }
    }
    Ok(())
}

fn validate_mount_coverage(
    gate: PhysicalStudyGate,
    participant: &str,
    mount: &str,
    rows: &[&PhysicalStudyEvidenceRow],
) -> Result<(), PhysicalStudyError> {
    for label in physical_class_labels(gate) {
        let count = rows
            .iter()
            .filter(|row| row.truth_label.as_deref() == Some(label.as_str()))
            .count();
        if count < usize::try_from(MIN_EVENTS_PER_CLASS_PER_MOUNT).unwrap_or(usize::MAX) {
            return Err(PhysicalStudyError::MissingStratum(format!(
                "participant {participant}, mount {mount}, class {label} has {count}; requires {MIN_EVENTS_PER_CLASS_PER_MOUNT}"
            )));
        }
    }

    match gate {
        PhysicalStudyGate::Gate0B => {
            let quiet = exposure_us(rows, StudyCondition::Quiet)?;
            let scripted = exposure_us(rows, StudyCondition::ScriptedMotion)?;
            if quiet < MIN_NEGATIVE_EXPOSURE_US_PER_CONDITION
                || scripted < MIN_NEGATIVE_EXPOSURE_US_PER_CONDITION
                || quiet != scripted
            {
                return Err(PhysicalStudyError::MissingStratum(format!(
                    "participant {participant}, mount {mount} needs equal quiet and scripted-motion exposure of at least {MIN_NEGATIVE_EXPOSURE_US_PER_CONDITION} microseconds each"
                )));
            }
        }
        PhysicalStudyGate::Gate1 => {
            for condition in [
                StudyCondition::Quiet,
                StudyCondition::SharedMotion,
                StudyCondition::PhysicalKeyboardTyping,
                StudyCondition::DeskTapping,
                StudyCondition::SimultaneousAmbiguous,
            ] {
                if exposure_us(rows, condition)? < MIN_NEGATIVE_EXPOSURE_US_PER_CONDITION {
                    return Err(PhysicalStudyError::MissingStratum(format!(
                        "participant {participant}, mount {mount} needs at least {MIN_NEGATIVE_EXPOSURE_US_PER_CONDITION} microseconds of {condition:?} exposure"
                    )));
                }
            }
        }
        PhysicalStudyGate::Gate2 => {
            if rows
                .iter()
                .any(|row| row.condition != StudyCondition::IntentionalTap)
            {
                return Err(PhysicalStudyError::InvalidField {
                    row_id: format!("{participant}/{mount}"),
                    message: "Gate 2 accepts only randomized physical-position trials".to_owned(),
                });
            }
        }
    }
    Ok(())
}

fn exposure_us(
    rows: &[&PhysicalStudyEvidenceRow],
    condition: StudyCondition,
) -> Result<u64, PhysicalStudyError> {
    rows.iter()
        .filter(|row| row.condition == condition)
        .try_fold(0_u64, |total, row| {
            total
                .checked_add(row.clock.truth_end_us - row.clock.truth_start_us)
                .ok_or_else(|| PhysicalStudyError::Arithmetic("exposure overflow".to_owned()))
        })
}

fn fit_centroids(
    rows: &[PhysicalStudyEvidenceRow],
    gate: PhysicalStudyGate,
    class_labels: &[String],
) -> Result<CentroidModel, PhysicalStudyError> {
    let dimensions = rows
        .iter()
        .flat_map(|row| &row.candidate_events)
        .next()
        .map(|event| event.features.len())
        .ok_or_else(|| PhysicalStudyError::MissingStratum("no classifier features".to_owned()))?;
    let mut classes = class_labels
        .iter()
        .map(|label| CentroidClass {
            label: label.clone(),
            count: 0,
            sums: vec![0; dimensions],
        })
        .collect::<Vec<_>>();

    for row in rows.iter().filter(|row| row.split == StudySplit::Train) {
        let Some(label) = row.truth_label.as_deref() else {
            continue;
        };
        let Some(event) = primary_candidate(row) else {
            continue;
        };
        let class_index = class_labels
            .iter()
            .position(|candidate| candidate == label)
            .ok_or_else(|| PhysicalStudyError::InvalidField {
                row_id: row.clock.prompt_id.clone(),
                message: format!("{label} is not a {gate:?} class"),
            })?;
        let class = &mut classes[class_index];
        class.count = class
            .count
            .checked_add(1)
            .ok_or_else(|| PhysicalStudyError::Arithmetic("class count overflow".to_owned()))?;
        for (sum, value) in class.sums.iter_mut().zip(&event.features) {
            *sum = sum.checked_add(i64::from(*value)).ok_or_else(|| {
                PhysicalStudyError::Arithmetic("centroid sum overflow".to_owned())
            })?;
        }
    }
    if let Some(class) = classes.iter().find(|class| class.count == 0) {
        return Err(PhysicalStudyError::MissingStratum(format!(
            "training has no matched event for {}",
            class.label
        )));
    }
    Ok(CentroidModel::new(CentroidModelArtifact {
        schema: CENTROID_MODEL_SCHEMA,
        gate,
        feature_dimensions: dimensions,
        classes,
        threshold_squared_distance_bits: 0,
    }))
}

fn select_validation_threshold(
    rows: &[PhysicalStudyEvidenceRow],
    model: &CentroidModel,
) -> Result<f64, PhysicalStudyError> {
    let mut correct_class_distances = Vec::new();
    let mut negative_distances = Vec::new();
    for row in rows
        .iter()
        .filter(|row| row.split == StudySplit::Validation)
    {
        if let Some(truth_label) = row.truth_label.as_deref() {
            if let Some(event) = primary_candidate(row) {
                let (class_index, distance) = model.nearest(&event.features);
                if model.artifact.classes[class_index].label == truth_label {
                    correct_class_distances.push(distance);
                }
            }
        } else {
            negative_distances.extend(
                row.candidate_events
                    .iter()
                    .map(|event| model.nearest(&event.features).1),
            );
        }
    }
    correct_class_distances.sort_by(f64::total_cmp);
    let positive_p95 = percentile_f64(&correct_class_distances, 95).ok_or_else(|| {
        PhysicalStudyError::MissingStratum(
            "validation has no correctly classified matched event".to_owned(),
        )
    })?;
    let negative_floor = negative_distances.into_iter().min_by(f64::total_cmp);
    Ok(negative_floor.map_or(positive_p95, |floor| {
        if floor > 0.0 {
            positive_p95.min(f64::from_bits(floor.to_bits() - 1))
        } else {
            // A zero-distance negative is inseparable from a centroid. No
            // nonnegative threshold can reject it, so freeze an abstain-all
            // threshold and let heldout identity metrics fail closed.
            -f64::MIN_POSITIVE
        }
    }))
}

#[derive(Clone, Debug)]
struct ScoredRows {
    labels: Vec<String>,
    confusion: Vec<Vec<u64>>,
    positive_trials: u64,
    negative_trials: u64,
    negative_exposure_us: u64,
    matched_events: u64,
    misses: u64,
    extra_events: u64,
    duplicate_events: u64,
    false_activations: u64,
    event_errors: u64,
    latencies_us: Vec<i64>,
}

/// Heldout model outputs keyed by immutable event ID. Group reducers scan rows
/// more than once, but the expensive centroid decision happens exactly once.
#[derive(Clone, Debug)]
struct ClassificationCache<'a> {
    output_indices: BTreeMap<&'a str, Option<usize>>,
}

impl ClassificationCache<'_> {
    fn output_index(&self, event_id: &str) -> Option<usize> {
        self.output_indices
            .get(event_id)
            .copied()
            .expect("validated heldout events are cached")
    }
}

fn scoring_labels(gate: PhysicalStudyGate, class_labels: &[String]) -> Vec<String> {
    match gate {
        PhysicalStudyGate::Gate0B => class_labels.to_vec(),
        PhysicalStudyGate::Gate1 => {
            let mut labels = class_labels.to_vec();
            labels.push("null".to_owned());
            labels
        }
        PhysicalStudyGate::Gate2 => {
            let mut labels = advertised_labels(gate);
            labels.push("null".to_owned());
            labels
        }
    }
}

fn build_classification_cache<'a>(
    rows: &[&'a PhysicalStudyEvidenceRow],
    gate: PhysicalStudyGate,
    model: &CentroidModel,
    class_labels: &[String],
) -> ClassificationCache<'a> {
    let labels = scoring_labels(gate, class_labels);
    let output_indices = rows
        .iter()
        .flat_map(|row| &row.candidate_events)
        .map(|event| {
            let output_index = model
                .classify(&event.features)
                .and_then(|label| output_label(gate, label))
                .map(|label| {
                    labels
                        .iter()
                        .position(|candidate| candidate == label)
                        .expect("model output is in the scoring labels")
                });
            (event.event_id.as_str(), output_index)
        })
        .collect();
    ClassificationCache { output_indices }
}

fn score_rows(
    rows: &[&PhysicalStudyEvidenceRow],
    gate: PhysicalStudyGate,
    class_labels: &[String],
    classification_cache: Option<&ClassificationCache<'_>>,
) -> Result<ScoredRows, PhysicalStudyError> {
    let labels = scoring_labels(gate, class_labels);
    let mut scored = ScoredRows {
        confusion: vec![vec![0; labels.len()]; labels.len()],
        labels,
        positive_trials: 0,
        negative_trials: 0,
        negative_exposure_us: 0,
        matched_events: 0,
        misses: 0,
        extra_events: 0,
        duplicate_events: 0,
        false_activations: 0,
        event_errors: 0,
        latencies_us: Vec::new(),
    };

    for row in rows {
        if row.condition == StudyCondition::IntentionalTap {
            scored.positive_trials += 1;
        } else {
            scored.negative_trials += 1;
            scored.negative_exposure_us = scored
                .negative_exposure_us
                .checked_add(row.clock.truth_end_us - row.clock.truth_start_us)
                .ok_or_else(|| PhysicalStudyError::Arithmetic("exposure overflow".to_owned()))?;
        }

        if gate == PhysicalStudyGate::Gate0B {
            score_contact_row(row, &mut scored)?;
        } else {
            score_classification_row(
                row,
                gate,
                classification_cache.expect("classifier gate has cached outputs"),
                &mut scored,
            )?;
        }
    }
    Ok(scored)
}

fn score_contact_row(
    row: &PhysicalStudyEvidenceRow,
    scored: &mut ScoredRows,
) -> Result<(), PhysicalStudyError> {
    if let Some(truth_time) = row.clock.truth_event_time_us {
        if let Some(primary) = matching_candidates(row).next() {
            scored.matched_events += 1;
            scored
                .latencies_us
                .push(signed_delta(primary.event_time_us, truth_time)?);
            let extras = row.candidate_events.len().saturating_sub(1);
            let extras = u64::try_from(extras).map_err(|_| {
                PhysicalStudyError::Arithmetic("extra event count overflow".to_owned())
            })?;
            scored.extra_events += extras;
            scored.duplicate_events += extras;
        } else {
            scored.misses += 1;
            scored.extra_events += u64::try_from(row.candidate_events.len()).map_err(|_| {
                PhysicalStudyError::Arithmetic("extra event count overflow".to_owned())
            })?;
        }
    } else {
        let false_count = u64::try_from(row.candidate_events.len())
            .map_err(|_| PhysicalStudyError::Arithmetic("false event count overflow".to_owned()))?;
        scored.false_activations += false_count;
        scored.extra_events += false_count;
    }
    Ok(())
}

fn score_classification_row(
    row: &PhysicalStudyEvidenceRow,
    gate: PhysicalStudyGate,
    classification_cache: &ClassificationCache<'_>,
    scored: &mut ScoredRows,
) -> Result<(), PhysicalStudyError> {
    let expected = row
        .truth_label
        .as_deref()
        .and_then(|label| output_label(gate, label));
    let null_index = scored.labels.len() - 1;
    let truth_index = expected.map_or(null_index, |truth| {
        scored
            .labels
            .iter()
            .position(|label| label == truth)
            .expect("output truth label is in the confusion matrix")
    });
    let mut output_count = 0_u64;
    let mut primary = None;
    for event in &row.candidate_events {
        let Some(output_index) = classification_cache.output_index(&event.event_id) else {
            continue;
        };
        output_count = output_count
            .checked_add(1)
            .ok_or_else(|| PhysicalStudyError::Arithmetic("output count overflow".to_owned()))?;
        if primary.is_none()
            && (row.clock.truth_event_time_us.is_none()
                || candidate_matches(row, event.event_time_us))
        {
            primary = Some((output_index, event.event_time_us));
        }
    }
    let predicted_index = primary.map_or(null_index, |(index, _)| index);
    scored.confusion[truth_index][predicted_index] += 1;

    let matched_count = u64::from(expected.is_some() && primary.is_some());
    scored.matched_events += matched_count;
    scored.extra_events += output_count.saturating_sub(matched_count);
    if expected.is_some() {
        scored.duplicate_events += output_count.saturating_sub(matched_count);
    }
    if expected.is_some() && primary.is_none() {
        scored.misses += 1;
    }
    if let (Some(_), Some((_, event_time)), Some(truth_time)) =
        (expected, primary, row.clock.truth_event_time_us)
    {
        scored
            .latencies_us
            .push(signed_delta(event_time, truth_time)?);
    }
    if expected.is_none() {
        scored.false_activations += output_count;
    }
    if truth_index != predicted_index || output_count > matched_count {
        scored.event_errors += 1;
    }
    Ok(())
}

fn primary_candidate(row: &PhysicalStudyEvidenceRow) -> Option<&CandidateObservation> {
    matching_candidates(row).next()
}

fn matching_candidates(
    row: &PhysicalStudyEvidenceRow,
) -> impl Iterator<Item = &CandidateObservation> {
    row.candidate_events
        .iter()
        .filter(|event| candidate_matches(row, event.event_time_us))
}

fn candidate_matches(row: &PhysicalStudyEvidenceRow, event_time_us: u64) -> bool {
    row.clock.truth_event_time_us.is_some_and(|truth_time| {
        // Row admission guarantees that the complete matching window is representable.
        event_time_us >= truth_time - MATCH_BEFORE_US
            && event_time_us <= truth_time + MATCH_AFTER_US
    })
}

fn signed_delta(event_time_us: u64, truth_time_us: u64) -> Result<i64, PhysicalStudyError> {
    let delta = i128::from(event_time_us) - i128::from(truth_time_us);
    i64::try_from(delta)
        .map_err(|_| PhysicalStudyError::Arithmetic("latency does not fit i64".to_owned()))
}

fn output_label(gate: PhysicalStudyGate, physical_label: &str) -> Option<&str> {
    match gate {
        PhysicalStudyGate::Gate0B | PhysicalStudyGate::Gate1 => Some(physical_label),
        PhysicalStudyGate::Gate2 => slot_for_label(physical_label)
            .and_then(|slot| slot.usage)
            .map(|_| physical_label),
    }
}

fn build_metrics(
    rows: &[&PhysicalStudyEvidenceRow],
    gate: PhysicalStudyGate,
    class_labels: &[String],
    classification_cache: Option<&ClassificationCache<'_>>,
    scored: &ScoredRows,
) -> Result<PhysicalStudyMetrics, PhysicalStudyError> {
    let participant_rates =
        participant_rate_samples(rows, gate, class_labels, classification_cache)?;
    let miss_samples = participant_rates
        .iter()
        .map(|rates| rates.miss_rate)
        .collect::<Vec<_>>();
    let miss_upper_samples = participant_rates
        .iter()
        .map(|rates| rates.miss_rate_upper_95)
        .collect::<Vec<_>>();
    let miss_rate_ppm = Some(floor_ppm(mean(&miss_samples)));
    let miss_upper_95_ppm = Some(bootstrap_upper_95_ppm(&miss_upper_samples));
    let event_samples = participant_rates
        .iter()
        .filter_map(|rates| rates.event_error_rate)
        .collect::<Vec<_>>();
    let event_upper_samples = participant_rates
        .iter()
        .filter_map(|rates| rates.event_error_rate_upper_95)
        .collect::<Vec<_>>();
    let event_error_rate_ppm = (!event_samples.is_empty()).then(|| floor_ppm(mean(&event_samples)));
    let event_error_upper_95_ppm =
        (!event_upper_samples.is_empty()).then(|| bootstrap_upper_95_ppm(&event_upper_samples));
    let false_rate = (scored.negative_exposure_us > 0).then(|| {
        let milli_events_per_hour =
            u128::from(scored.false_activations) * 3_600_000_000_u128 * 1_000_u128;
        let rounded_up = milli_events_per_hour.div_ceil(u128::from(scored.negative_exposure_us));
        u64::try_from(rounded_up).unwrap_or(u64::MAX)
    });

    let (per_class, confusion_matrix, participant_macro, participant_macro_lower) =
        if gate == PhysicalStudyGate::Gate0B {
            (contact_class_metrics(rows), None, None, None)
        } else {
            let advertised = advertised_labels(gate);
            let mut metrics = class_metrics(&scored.confusion, &scored.labels, &advertised);
            if gate == PhysicalStudyGate::Gate2 {
                metrics.extend(reserved_class_metrics(
                    rows,
                    classification_cache.expect("Gate 2 has cached outputs"),
                )?);
            }
            let participant_scores = participant_rates
                .iter()
                .filter_map(|rates| rates.macro_f1)
                .collect::<Vec<_>>();
            let participant_lower_scores = participant_rates
                .iter()
                .filter_map(|rates| rates.macro_f1_lower_95)
                .collect::<Vec<_>>();
            let point = mean(&participant_scores);
            (
                metrics,
                Some(ConfusionMatrix {
                    labels: scored.labels.clone(),
                    counts: scored.confusion.clone(),
                }),
                Some(floor_ppm(point)),
                Some(bootstrap_lower_95_ppm(&participant_lower_scores)),
            )
        };

    let latency_min_us = scored.latencies_us.iter().min().copied();
    let latency_p95_samples = participant_rates
        .iter()
        .filter_map(|rates| rates.latency_p95_us)
        .collect::<Vec<_>>();
    let latency_p95_upper_samples = participant_rates
        .iter()
        .filter_map(|rates| rates.latency_p95_upper_95_us)
        .collect::<Vec<_>>();
    let latency_p99_samples = participant_rates
        .iter()
        .filter_map(|rates| rates.latency_p99_us)
        .collect::<Vec<_>>();
    let latency_p99_upper_samples = participant_rates
        .iter()
        .filter_map(|rates| rates.latency_p99_upper_95_us)
        .collect::<Vec<_>>();
    let complete_latency_p95 = latency_p95_samples.len() == participant_rates.len()
        && latency_p95_upper_samples.len() == participant_rates.len();
    let complete_latency_p99 = latency_p99_samples.len() == participant_rates.len()
        && latency_p99_upper_samples.len() == participant_rates.len();
    let latency_p95_us = complete_latency_p95.then(|| integer_mean_ceiling(&latency_p95_samples));
    let latency_p95_upper_95_us =
        complete_latency_p95.then(|| bootstrap_upper_95_i64(&latency_p95_upper_samples));
    let latency_p99_us = complete_latency_p99.then(|| integer_mean_ceiling(&latency_p99_samples));
    let latency_p99_upper_95_us =
        complete_latency_p99.then(|| bootstrap_upper_95_i64(&latency_p99_upper_samples));
    let participant_minimum_class_recall_ppm = Some(floor_ppm(
        minimum_participant_balanced_class_recall(&participant_rates),
    ));
    let participant_minimum_class_recall_lower_95_ppm =
        Some(bootstrap_lower_95_minimum_class_recall(&participant_rates));

    Ok(PhysicalStudyMetrics {
        positive_trials: scored.positive_trials,
        negative_trials: scored.negative_trials,
        negative_exposure_us: scored.negative_exposure_us,
        matched_events: scored.matched_events,
        misses: scored.misses,
        extra_events: scored.extra_events,
        duplicate_events: scored.duplicate_events,
        false_activations: scored.false_activations,
        false_activations_milli_per_hour: false_rate,
        event_errors: scored.event_errors,
        miss_rate_ppm,
        miss_upper_95_ppm,
        event_error_rate_ppm,
        event_error_upper_95_ppm,
        latency_min_us,
        latency_p95_us,
        latency_p95_upper_95_us,
        latency_p99_us,
        latency_p99_upper_95_us,
        participant_minimum_class_recall_ppm,
        participant_minimum_class_recall_lower_95_ppm,
        participant_macro_f1_ppm: participant_macro,
        participant_macro_f1_lower_95_ppm: participant_macro_lower,
        per_class,
        confusion_matrix,
    })
}

#[derive(Clone, Debug)]
struct ParticipantRates {
    miss_rate: f64,
    miss_rate_upper_95: f64,
    event_error_rate: Option<f64>,
    event_error_rate_upper_95: Option<f64>,
    macro_f1: Option<f64>,
    macro_f1_lower_95: Option<f64>,
    class_recalls: Vec<f64>,
    class_recall_lower_95: Vec<f64>,
    latency_p95_us: Option<i64>,
    latency_p95_upper_95_us: Option<i64>,
    latency_p99_us: Option<i64>,
    latency_p99_upper_95_us: Option<i64>,
}

fn participant_rate_samples(
    rows: &[&PhysicalStudyEvidenceRow],
    gate: PhysicalStudyGate,
    class_labels: &[String],
    classification_cache: Option<&ClassificationCache<'_>>,
) -> Result<Vec<ParticipantRates>, PhysicalStudyError> {
    let participants = rows
        .iter()
        .map(|row| row.participant_id.as_str())
        .collect::<BTreeSet<_>>();
    participants
        .into_iter()
        .map(|participant| {
            let participant_rows = rows
                .iter()
                .copied()
                .filter(|row| row.participant_id == participant)
                .collect::<Vec<_>>();
            let scored = score_rows(&participant_rows, gate, class_labels, classification_cache)?;
            let miss_rate = scored.misses as f64 / scored.positive_trials as f64;
            let alpha = f64::from(ONE_SIDED_ALPHA_PPM) / RATE_SCALE as f64;
            let miss_rate_upper_95 =
                clopper_pearson_upper(scored.misses, scored.positive_trials, alpha);
            let row_count = u64::try_from(participant_rows.len()).map_err(|_| {
                PhysicalStudyError::Arithmetic("participant row count overflow".to_owned())
            })?;
            let event_error_rate = (gate != PhysicalStudyGate::Gate0B)
                .then(|| scored.event_errors as f64 / row_count as f64);
            let event_error_rate_upper_95 = (gate != PhysicalStudyGate::Gate0B)
                .then(|| clopper_pearson_upper(scored.event_errors, row_count, alpha));
            let (macro_f1, macro_f1_lower_95) = if gate == PhysicalStudyGate::Gate0B {
                (None, None)
            } else {
                let advertised = advertised_labels(gate);
                (
                    Some(macro_f1(&scored.confusion, &scored.labels, &advertised)),
                    Some(conservative_macro_f1_lower(
                        &scored.confusion,
                        &scored.labels,
                        &advertised,
                        alpha,
                    )),
                )
            };
            let physical_metrics = participant_physical_class_metrics(
                &participant_rows,
                gate,
                classification_cache,
                &scored,
                class_labels,
            )?;
            let class_alpha = alpha / physical_metrics.len() as f64;
            let class_recalls = physical_metrics
                .iter()
                .map(|metric| metric.true_positive as f64 / metric.support as f64)
                .collect();
            let class_recall_lower_95 = physical_metrics
                .iter()
                .map(|metric| {
                    clopper_pearson_lower(metric.true_positive, metric.support, class_alpha)
                })
                .collect();
            let mut latencies = scored.latencies_us;
            latencies.sort_unstable();
            let latency_p95_us = percentile_sorted_i64(&latencies, 95);
            let latency_p95_upper_95_us = quantile_upper_confidence_bound(&latencies, 95, alpha);
            let latency_p99_us = percentile_sorted_i64(&latencies, 99);
            let latency_p99_upper_95_us = quantile_upper_confidence_bound(&latencies, 99, alpha);
            Ok(ParticipantRates {
                miss_rate,
                miss_rate_upper_95,
                event_error_rate,
                event_error_rate_upper_95,
                macro_f1,
                macro_f1_lower_95,
                class_recalls,
                class_recall_lower_95,
                latency_p95_us,
                latency_p95_upper_95_us,
                latency_p99_us,
                latency_p99_upper_95_us,
            })
        })
        .collect()
}

fn participant_physical_class_metrics(
    rows: &[&PhysicalStudyEvidenceRow],
    gate: PhysicalStudyGate,
    classification_cache: Option<&ClassificationCache<'_>>,
    scored: &ScoredRows,
    class_labels: &[String],
) -> Result<Vec<ClassMetric>, PhysicalStudyError> {
    let metrics = match gate {
        PhysicalStudyGate::Gate0B => contact_class_metrics(rows),
        PhysicalStudyGate::Gate1 => class_metrics(&scored.confusion, &scored.labels, class_labels),
        PhysicalStudyGate::Gate2 => {
            let mut metrics =
                class_metrics(&scored.confusion, &scored.labels, &advertised_labels(gate));
            metrics.extend(reserved_class_metrics(
                rows,
                classification_cache.expect("Gate 2 has cached outputs"),
            )?);
            metrics
        }
    };
    let mut metrics_by_label = metrics
        .into_iter()
        .map(|metric| (metric.label.clone(), metric))
        .collect::<BTreeMap<_, _>>();
    class_labels
        .iter()
        .map(|label| {
            metrics_by_label.remove(label).ok_or_else(|| {
                PhysicalStudyError::MissingStratum(format!(
                    "participant metrics lack physical class {label}"
                ))
            })
        })
        .collect()
}

fn contact_class_metrics(rows: &[&PhysicalStudyEvidenceRow]) -> Vec<ClassMetric> {
    physical_class_labels(PhysicalStudyGate::Gate0B)
        .into_iter()
        .map(|label| {
            let class_rows = rows
                .iter()
                .filter(|row| row.truth_label.as_deref() == Some(label.as_str()))
                .collect::<Vec<_>>();
            let support = class_rows.len() as u64;
            let true_positive = class_rows
                .iter()
                .filter(|row| primary_candidate(row).is_some())
                .count() as u64;
            let false_negative = support - true_positive;
            let recall = ratio_ppm(true_positive, support).unwrap_or(0);
            ClassMetric {
                label,
                support,
                true_positive,
                false_positive: 0,
                false_negative,
                precision_ppm: u32::try_from(RATE_SCALE).expect("rate scale fits u32"),
                recall_ppm: recall,
                f1_ppm: if true_positive == 0 {
                    0
                } else {
                    ratio_ppm(2 * true_positive, 2 * true_positive + false_negative).unwrap_or(0)
                },
            }
        })
        .collect()
}

fn class_metrics(
    confusion: &[Vec<u64>],
    labels: &[String],
    reported_labels: &[String],
) -> Vec<ClassMetric> {
    reported_labels
        .iter()
        .map(|label| {
            let index = labels
                .iter()
                .position(|candidate| candidate == label)
                .expect("reported class is in matrix");
            let true_positive = confusion[index][index];
            let support = confusion[index].iter().sum();
            let predicted: u64 = confusion.iter().map(|row| row[index]).sum();
            let false_positive = predicted - true_positive;
            let false_negative = support - true_positive;
            let precision_ppm = ratio_ppm(true_positive, predicted).unwrap_or(0);
            let recall_ppm = ratio_ppm(true_positive, support).unwrap_or(0);
            let f1_ppm = ratio_ppm(
                2 * true_positive,
                2 * true_positive + false_positive + false_negative,
            )
            .unwrap_or(0);
            ClassMetric {
                label: label.clone(),
                support,
                true_positive,
                false_positive,
                false_negative,
                precision_ppm,
                recall_ppm,
                f1_ppm,
            }
        })
        .collect()
}

fn reserved_class_metrics(
    rows: &[&PhysicalStudyEvidenceRow],
    classification_cache: &ClassificationCache<'_>,
) -> Result<Vec<ClassMetric>, PhysicalStudyError> {
    all_slots()
        .iter()
        .filter(|slot| slot.usage.is_none())
        .map(|slot| {
            let class_rows = rows
                .iter()
                .copied()
                .filter(|row| row.truth_label.as_deref() == Some(slot.label))
                .collect::<Vec<_>>();
            let support = u64::try_from(class_rows.len()).map_err(|_| {
                PhysicalStudyError::Arithmetic("reserved support overflow".to_owned())
            })?;
            let mut correct = 0_u64;
            for row in class_rows {
                let emitted = row
                    .candidate_events
                    .iter()
                    .any(|event| classification_cache.output_index(&event.event_id).is_some());
                if !emitted {
                    correct += 1;
                }
            }
            let incorrect = support - correct;
            let recall_ppm = ratio_ppm(correct, support).unwrap_or(0);
            Ok(ClassMetric {
                label: slot.label.to_owned(),
                support,
                true_positive: correct,
                false_positive: 0,
                false_negative: incorrect,
                precision_ppm: u32::try_from(RATE_SCALE).expect("rate scale fits u32"),
                recall_ppm,
                f1_ppm: if correct == 0 {
                    0
                } else {
                    ratio_ppm(2 * correct, 2 * correct + incorrect).unwrap_or(0)
                },
            })
        })
        .collect()
}

fn macro_f1(confusion: &[Vec<u64>], labels: &[String], classes: &[String]) -> f64 {
    let metrics = class_metrics(confusion, labels, classes);
    metrics
        .iter()
        .map(|metric| f64::from(metric.f1_ppm) / RATE_SCALE as f64)
        .sum::<f64>()
        / metrics.len() as f64
}

fn conservative_macro_f1_lower(
    confusion: &[Vec<u64>],
    labels: &[String],
    classes: &[String],
    alpha: f64,
) -> f64 {
    let metrics = class_metrics(confusion, labels, classes);
    let component_alpha = alpha / (2 * metrics.len()) as f64;
    metrics
        .iter()
        .map(|metric| {
            let predicted = metric.true_positive + metric.false_positive;
            let precision_lower = if predicted > 0 {
                clopper_pearson_lower(metric.true_positive, predicted, component_alpha)
            } else {
                0.0
            };
            let recall_lower =
                clopper_pearson_lower(metric.true_positive, metric.support, component_alpha);
            if precision_lower == 0.0 || recall_lower == 0.0 {
                0.0
            } else {
                2.0 * precision_lower * recall_lower / (precision_lower + recall_lower)
            }
        })
        .sum::<f64>()
        / metrics.len() as f64
}

fn build_criteria(gate: PhysicalStudyGate, metrics: &PhysicalStudyMetrics) -> Vec<CriterionReport> {
    match gate {
        PhysicalStudyGate::Gate0B => {
            let miss_point = i64::from(metrics.miss_rate_ppm.unwrap_or(u32::MAX));
            let miss_bound = i64::from(metrics.miss_upper_95_ppm.unwrap_or(u32::MAX));
            let min_recall = i64::from(metrics.participant_minimum_class_recall_ppm.unwrap_or(0));
            let min_recall_bound = i64::from(
                metrics
                    .participant_minimum_class_recall_lower_95_ppm
                    .unwrap_or(0),
            );
            vec![
                upper_bound_criterion(
                    "miss-rate-upper-95",
                    miss_point,
                    miss_bound,
                    i64::from(MISS_UPPER_LIMIT_PPM),
                    "ppm",
                    "participant bootstrap over one-sided exact per-participant binomial bounds, 20000 replicates",
                ),
                point_at_least(
                    "contact-latency-minimum",
                    metrics.latency_min_us.unwrap_or(i64::MIN),
                    MIN_CONTACT_LATENCY_US,
                    "microseconds",
                ),
                lower_bound_criterion(
                    "minimum-finger-recall-lower-95",
                    min_recall,
                    min_recall_bound,
                    i64::from(MIN_RECALL_PPM),
                    "ppm",
                    "participant bootstrap over Bonferroni-adjusted one-sided exact per-participant class bounds, 20000 replicates",
                ),
                upper_bound_criterion(
                    "contact-latency-p95-upper-95",
                    metrics.latency_p95_us.unwrap_or(i64::MAX),
                    metrics.latency_p95_upper_95_us.unwrap_or(i64::MAX),
                    LATENCY_P95_LIMIT_US,
                    "microseconds",
                    "participant bootstrap over exact one-sided nonparametric per-participant quantile bounds, 20000 replicates",
                ),
                upper_bound_criterion(
                    "contact-latency-p99-upper-95",
                    metrics.latency_p99_us.unwrap_or(i64::MAX),
                    metrics.latency_p99_upper_95_us.unwrap_or(i64::MAX),
                    LATENCY_P99_LIMIT_US,
                    "microseconds",
                    "participant bootstrap over exact one-sided nonparametric per-participant quantile bounds, 20000 replicates",
                ),
                point_at_most(
                    "extra-contact-events",
                    i64::try_from(metrics.extra_events).unwrap_or(i64::MAX),
                    0,
                    "events",
                ),
                point_at_most(
                    "negative-false-activation-rate",
                    i64::try_from(metrics.false_activations_milli_per_hour.unwrap_or(u64::MAX))
                        .unwrap_or(i64::MAX),
                    0,
                    "milli-events-per-hour",
                ),
            ]
        }
        PhysicalStudyGate::Gate1 | PhysicalStudyGate::Gate2 => {
            let macro_point = i64::from(metrics.participant_macro_f1_ppm.unwrap_or(0));
            let macro_bound = i64::from(metrics.participant_macro_f1_lower_95_ppm.unwrap_or(0));
            let error_point = i64::from(metrics.event_error_rate_ppm.unwrap_or(u32::MAX));
            let error_bound = i64::from(metrics.event_error_upper_95_ppm.unwrap_or(u32::MAX));
            let min_recall = i64::from(metrics.participant_minimum_class_recall_ppm.unwrap_or(0));
            let min_recall_bound = i64::from(
                metrics
                    .participant_minimum_class_recall_lower_95_ppm
                    .unwrap_or(0),
            );
            let (false_id, false_point, false_unit) = if gate == PhysicalStudyGate::Gate1 {
                (
                    "negative-false-activation-rate",
                    i64::try_from(metrics.false_activations_milli_per_hour.unwrap_or(u64::MAX))
                        .unwrap_or(i64::MAX),
                    "milli-events-per-hour",
                )
            } else {
                (
                    "reserved-position-output-events",
                    i64::try_from(metrics.false_activations).unwrap_or(i64::MAX),
                    "events",
                )
            };
            vec![
                lower_bound_criterion(
                    "participant-macro-f1-lower-95",
                    macro_point,
                    macro_bound,
                    i64::from(MIN_MACRO_F1_PPM),
                    "ppm",
                    "participant bootstrap over Bonferroni-adjusted one-sided exact per-participant precision and recall bounds, 20000 replicates",
                ),
                upper_bound_criterion(
                    "event-error-upper-95",
                    error_point,
                    error_bound,
                    i64::from(EVENT_ERROR_UPPER_LIMIT_PPM),
                    "ppm",
                    "participant bootstrap over one-sided exact per-participant binomial bounds, 20000 replicates",
                ),
                point_at_least(
                    "output-latency-minimum",
                    metrics.latency_min_us.unwrap_or(i64::MIN),
                    MIN_CONTACT_LATENCY_US,
                    "microseconds",
                ),
                upper_bound_criterion(
                    "output-latency-p95-upper-95",
                    metrics.latency_p95_us.unwrap_or(i64::MAX),
                    metrics.latency_p95_upper_95_us.unwrap_or(i64::MAX),
                    LATENCY_P95_LIMIT_US,
                    "microseconds",
                    "participant bootstrap over exact one-sided nonparametric per-participant quantile bounds, 20000 replicates",
                ),
                upper_bound_criterion(
                    "output-latency-p99-upper-95",
                    metrics.latency_p99_us.unwrap_or(i64::MAX),
                    metrics.latency_p99_upper_95_us.unwrap_or(i64::MAX),
                    LATENCY_P99_LIMIT_US,
                    "microseconds",
                    "participant bootstrap over exact one-sided nonparametric per-participant quantile bounds, 20000 replicates",
                ),
                lower_bound_criterion(
                    "minimum-physical-class-recall-lower-95",
                    min_recall,
                    min_recall_bound,
                    i64::from(MIN_RECALL_PPM),
                    "ppm",
                    "participant bootstrap over Bonferroni-adjusted one-sided exact per-participant class bounds, 20000 replicates",
                ),
                point_at_most(
                    "extra-output-events",
                    i64::try_from(metrics.extra_events).unwrap_or(i64::MAX),
                    0,
                    "events",
                ),
                point_at_most(false_id, false_point, 0, false_unit),
            ]
        }
    }
}

fn upper_bound_criterion(
    id: &str,
    point: i64,
    bound: i64,
    threshold: i64,
    unit: &str,
    method: &str,
) -> CriterionReport {
    let outcome = if point > threshold {
        CriterionOutcome::Fail
    } else if bound > threshold {
        CriterionOutcome::Inconclusive
    } else {
        CriterionOutcome::Pass
    };
    CriterionReport {
        id: id.to_owned(),
        comparison: "at-most".to_owned(),
        threshold,
        unit: unit.to_owned(),
        point,
        confidence_bound: Some(bound),
        confidence_method: Some(method.to_owned()),
        outcome,
    }
}

fn lower_bound_criterion(
    id: &str,
    point: i64,
    bound: i64,
    threshold: i64,
    unit: &str,
    method: &str,
) -> CriterionReport {
    let outcome = if point < threshold {
        CriterionOutcome::Fail
    } else if bound < threshold {
        CriterionOutcome::Inconclusive
    } else {
        CriterionOutcome::Pass
    };
    CriterionReport {
        id: id.to_owned(),
        comparison: "at-least".to_owned(),
        threshold,
        unit: unit.to_owned(),
        point,
        confidence_bound: Some(bound),
        confidence_method: Some(method.to_owned()),
        outcome,
    }
}

fn point_at_most(id: &str, point: i64, threshold: i64, unit: &str) -> CriterionReport {
    CriterionReport {
        id: id.to_owned(),
        comparison: "at-most".to_owned(),
        threshold,
        unit: unit.to_owned(),
        point,
        confidence_bound: None,
        confidence_method: None,
        outcome: if point <= threshold {
            CriterionOutcome::Pass
        } else {
            CriterionOutcome::Fail
        },
    }
}

fn point_at_least(id: &str, point: i64, threshold: i64, unit: &str) -> CriterionReport {
    CriterionReport {
        id: id.to_owned(),
        comparison: "at-least".to_owned(),
        threshold,
        unit: unit.to_owned(),
        point,
        confidence_bound: None,
        confidence_method: None,
        outcome: if point >= threshold {
            CriterionOutcome::Pass
        } else {
            CriterionOutcome::Fail
        },
    }
}

fn disposition(criteria: &[CriterionReport]) -> GateDisposition {
    if criteria
        .iter()
        .any(|criterion| criterion.outcome == CriterionOutcome::Fail)
    {
        GateDisposition::NoGo
    } else if criteria
        .iter()
        .any(|criterion| criterion.outcome == CriterionOutcome::Inconclusive)
    {
        GateDisposition::Inconclusive
    } else {
        GateDisposition::Go
    }
}

fn build_groups(
    rows: &[&PhysicalStudyEvidenceRow],
    gate: PhysicalStudyGate,
    class_labels: &[String],
    classification_cache: Option<&ClassificationCache<'_>>,
) -> Result<(Vec<StudyGroupSummary>, Vec<StudyGroupSummary>), PhysicalStudyError> {
    let mut participant_map: BTreeMap<&str, Vec<&PhysicalStudyEvidenceRow>> = BTreeMap::new();
    let mut remount_map: BTreeMap<(&str, &str), Vec<&PhysicalStudyEvidenceRow>> = BTreeMap::new();
    for row in rows {
        participant_map
            .entry(&row.participant_id)
            .or_default()
            .push(row);
        remount_map
            .entry((&row.participant_id, &row.mount_id))
            .or_default()
            .push(row);
    }
    let participants = participant_map
        .into_iter()
        .map(|(participant, group)| {
            summarize_group(
                participant,
                None,
                &group,
                gate,
                class_labels,
                classification_cache,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    let remounts = remount_map
        .into_iter()
        .map(|((participant, mount), group)| {
            summarize_group(
                participant,
                Some(mount.to_owned()),
                &group,
                gate,
                class_labels,
                classification_cache,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok((participants, remounts))
}

fn summarize_group(
    participant: &str,
    mount: Option<String>,
    rows: &[&PhysicalStudyEvidenceRow],
    gate: PhysicalStudyGate,
    class_labels: &[String],
    classification_cache: Option<&ClassificationCache<'_>>,
) -> Result<StudyGroupSummary, PhysicalStudyError> {
    let scored = score_rows(rows, gate, class_labels, classification_cache)?;
    let macro_f1_ppm = (gate != PhysicalStudyGate::Gate0B).then(|| {
        floor_ppm(macro_f1(
            &scored.confusion,
            &scored.labels,
            &advertised_labels(gate),
        ))
    });
    Ok(StudyGroupSummary {
        participant_id: participant.to_owned(),
        mount_id: mount,
        surfaces: rows
            .iter()
            .map(|row| row.surface_id.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect(),
        sessions: rows
            .iter()
            .map(|row| row.session_id.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect(),
        positive_trials: scored.positive_trials,
        misses: scored.misses,
        event_errors: scored.event_errors,
        macro_f1_ppm,
    })
}

fn cohort_summary(rows: &[&PhysicalStudyEvidenceRow]) -> Result<CohortSummary, PhysicalStudyError> {
    let count = |values: BTreeSet<&str>| {
        u64::try_from(values.len())
            .map_err(|_| PhysicalStudyError::Arithmetic("cohort count overflow".to_owned()))
    };
    Ok(CohortSummary {
        participant_count: count(rows.iter().map(|row| row.participant_id.as_str()).collect())?,
        session_count: count(rows.iter().map(|row| row.session_id.as_str()).collect())?,
        mount_count: u64::try_from(
            rows.iter()
                .map(|row| (row.participant_id.as_str(), row.mount_id.as_str()))
                .collect::<BTreeSet<_>>()
                .len(),
        )
        .map_err(|_| PhysicalStudyError::Arithmetic("mount count overflow".to_owned()))?,
        rig_count: count(rows.iter().map(|row| row.rig_id.as_str()).collect())?,
        controller_count: count(rows.iter().map(|row| row.controller_id.as_str()).collect())?,
        surface_count: count(rows.iter().map(|row| row.surface_id.as_str()).collect())?,
    })
}

fn build_source_binding(
    rows: &[PhysicalStudyEvidenceRow],
    exact_jsonl_sha256: String,
) -> Result<StudySourceBinding, PhysicalStudyError> {
    Ok(StudySourceBinding {
        exact_jsonl_sha256,
        canonical_row_set_sha256: hash_row_set(rows.iter())?,
        train_row_set_sha256: split_row_set_hash(rows, StudySplit::Train)?,
        validation_row_set_sha256: split_row_set_hash(rows, StudySplit::Validation)?,
        heldout_row_set_sha256: split_row_set_hash(rows, StudySplit::Heldout)?
            .expect("heldout coverage was validated"),
        row_count: u64::try_from(rows.len())
            .map_err(|_| PhysicalStudyError::Arithmetic("row count overflow".to_owned()))?,
        observation_artifact_sha256: rows
            .iter()
            .map(|row| row.observation_source.artifact_sha256.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect(),
    })
}

fn split_row_set_hash(
    rows: &[PhysicalStudyEvidenceRow],
    split: StudySplit,
) -> Result<Option<String>, PhysicalStudyError> {
    let split_rows = rows
        .iter()
        .filter(|row| row.split == split)
        .collect::<Vec<_>>();
    if split_rows.is_empty() {
        Ok(None)
    } else {
        hash_row_set(split_rows.into_iter()).map(Some)
    }
}

fn hash_row_set<'a>(
    rows: impl Iterator<Item = &'a PhysicalStudyEvidenceRow>,
) -> Result<String, PhysicalStudyError> {
    let mut hashes = rows.map(|row| row.row_sha256.as_str()).collect::<Vec<_>>();
    hashes.sort_unstable();
    let mut bytes = Vec::with_capacity(hashes.len().saturating_mul(65));
    for hash in hashes {
        bytes.extend_from_slice(hash.as_bytes());
        bytes.push(b'\n');
    }
    if bytes.is_empty() {
        return Err(PhysicalStudyError::MissingStratum(
            "cannot hash an empty row set".to_owned(),
        ));
    }
    Ok(format_sha256(&sha256(&bytes)))
}

fn count_rows(
    rows: &[PhysicalStudyEvidenceRow],
    split: StudySplit,
) -> Result<u64, PhysicalStudyError> {
    u64::try_from(rows.iter().filter(|row| row.split == split).count())
        .map_err(|_| PhysicalStudyError::Arithmetic("row count overflow".to_owned()))
}

fn physical_class_labels(gate: PhysicalStudyGate) -> Vec<String> {
    match gate {
        PhysicalStudyGate::Gate0B | PhysicalStudyGate::Gate1 => {
            ["thumb", "index", "middle", "ring", "pinky"]
                .into_iter()
                .map(str::to_owned)
                .collect()
        }
        PhysicalStudyGate::Gate2 => all_slots()
            .iter()
            .map(|slot| slot.label.to_owned())
            .collect(),
    }
}

fn advertised_labels(gate: PhysicalStudyGate) -> Vec<String> {
    match gate {
        PhysicalStudyGate::Gate0B | PhysicalStudyGate::Gate1 => physical_class_labels(gate),
        PhysicalStudyGate::Gate2 => all_slots()
            .iter()
            .filter(|slot| slot.usage.is_some())
            .map(|slot| slot.label.to_owned())
            .collect(),
    }
}

fn ratio_ppm(numerator: u64, denominator: u64) -> Option<u32> {
    (denominator > 0).then(|| {
        let scaled = u128::from(numerator) * u128::from(RATE_SCALE) / u128::from(denominator);
        u32::try_from(scaled).unwrap_or(u32::MAX)
    })
}

fn floor_ppm(value: f64) -> u32 {
    if value <= 0.0 {
        0
    } else if value >= 1.0 {
        u32::try_from(RATE_SCALE).expect("rate scale fits u32")
    } else {
        (value * RATE_SCALE as f64).floor() as u32
    }
}

fn ceil_ppm(value: f64) -> u32 {
    if value <= 0.0 {
        0
    } else if value >= 1.0 {
        u32::try_from(RATE_SCALE).expect("rate scale fits u32")
    } else {
        (value * RATE_SCALE as f64).ceil() as u32
    }
}

fn bootstrap_lower_95_ppm(participant_scores: &[f64]) -> u32 {
    let means = bootstrap_means(participant_scores);
    floor_ppm(means[BOOTSTRAP_REPLICATES / 20])
}

fn bootstrap_upper_95_ppm(participant_scores: &[f64]) -> u32 {
    let means = bootstrap_means(participant_scores);
    ceil_ppm(means[(BOOTSTRAP_REPLICATES * 95 / 100).min(BOOTSTRAP_REPLICATES - 1)])
}

fn bootstrap_means(participant_scores: &[f64]) -> Vec<f64> {
    let mut state = 0x7265_6d6f_756e_7421_u64;
    let mut means = Vec::with_capacity(BOOTSTRAP_REPLICATES);
    for _ in 0..BOOTSTRAP_REPLICATES {
        let mut total = 0.0;
        for _ in participant_scores {
            let index = next_bootstrap_index(&mut state, participant_scores.len());
            total += participant_scores[index];
        }
        means.push(total / participant_scores.len() as f64);
    }
    means.sort_by(f64::total_cmp);
    means
}

fn integer_mean_ceiling(values: &[i64]) -> i64 {
    let total = values.iter().map(|value| i128::from(*value)).sum::<i128>();
    i64::try_from(divide_i128_ceiling(total, values.len() as i128))
        .expect("validated participant latency mean fits i64")
}

fn divide_i128_ceiling(numerator: i128, positive_denominator: i128) -> i128 {
    let quotient = numerator / positive_denominator;
    if numerator % positive_denominator > 0 {
        quotient + 1
    } else {
        quotient
    }
}

fn bootstrap_upper_95_i64(values: &[i64]) -> i64 {
    let mut state = 0x6c61_7465_6e63_7921_u64;
    let mut means = Vec::with_capacity(BOOTSTRAP_REPLICATES);
    for _ in 0..BOOTSTRAP_REPLICATES {
        let mut total = 0_i128;
        for _ in values {
            let index = next_bootstrap_index(&mut state, values.len());
            total += i128::from(values[index]);
        }
        means.push(
            i64::try_from(divide_i128_ceiling(total, values.len() as i128))
                .expect("validated bootstrap latency mean fits i64"),
        );
    }
    means.sort_unstable();
    means[(BOOTSTRAP_REPLICATES * 95 / 100).min(BOOTSTRAP_REPLICATES - 1)]
}

fn minimum_participant_balanced_class_recall(participant_rates: &[ParticipantRates]) -> f64 {
    let class_count = participant_rates[0].class_recalls.len();
    (0..class_count)
        .map(|class_index| {
            participant_rates
                .iter()
                .map(|rates| rates.class_recalls[class_index])
                .sum::<f64>()
                / participant_rates.len() as f64
        })
        .min_by(f64::total_cmp)
        .expect("validated studies have physical classes")
}

fn bootstrap_lower_95_minimum_class_recall(participant_rates: &[ParticipantRates]) -> u32 {
    let class_count = participant_rates[0].class_recall_lower_95.len();
    let mut state = 0x636c_6173_732d_6c62_u64;
    let mut minima = Vec::with_capacity(BOOTSTRAP_REPLICATES);
    let mut class_totals = vec![0.0; class_count];
    for _ in 0..BOOTSTRAP_REPLICATES {
        class_totals.fill(0.0);
        for _ in participant_rates {
            let index = next_bootstrap_index(&mut state, participant_rates.len());
            for (total, recall) in class_totals
                .iter_mut()
                .zip(&participant_rates[index].class_recall_lower_95)
            {
                *total += recall;
            }
        }
        minima.push(
            class_totals
                .iter()
                .map(|total| *total / participant_rates.len() as f64)
                .min_by(f64::total_cmp)
                .expect("validated studies have physical classes"),
        );
    }
    minima.sort_by(f64::total_cmp);
    floor_ppm(minima[BOOTSTRAP_REPLICATES / 20])
}

fn next_bootstrap_index(state: &mut u64, length: usize) -> usize {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    usize::try_from(*state % length as u64).expect("bootstrap index fits usize")
}

fn clopper_pearson_lower(successes: u64, trials: u64, alpha: f64) -> f64 {
    if successes == 0 {
        return 0.0;
    }
    if successes >= trials {
        return alpha.powf(1.0 / trials as f64);
    }
    let mut low = 0.0;
    let mut high = successes as f64 / trials as f64;
    for _ in 0..80 {
        let candidate = (low + high) / 2.0;
        if binomial_upper_tail(successes, trials, candidate) < alpha {
            low = candidate;
        } else {
            high = candidate;
        }
    }
    low
}

fn clopper_pearson_upper(successes: u64, trials: u64, alpha: f64) -> f64 {
    if successes >= trials {
        return 1.0;
    }
    if successes == 0 {
        return -(alpha.ln() / trials as f64).exp_m1();
    }
    let mut low = successes as f64 / trials as f64;
    let mut high = 1.0;
    for _ in 0..80 {
        let candidate = (low + high) / 2.0;
        if binomial_cdf(successes, trials, candidate) > alpha {
            low = candidate;
        } else {
            high = candidate;
        }
    }
    high
}

fn binomial_cdf(successes: u64, trials: u64, probability: f64) -> f64 {
    if probability <= 0.0 || successes >= trials {
        return 1.0;
    }
    if probability >= 1.0 {
        return 0.0;
    }
    let log_probability = probability.ln();
    let log_failure = (-probability).ln_1p();
    let mut log_term = trials as f64 * log_failure;
    let mut log_total = log_term;
    let mut count = 0_u64;
    while count < successes {
        log_term += ((trials - count) as f64).ln() - ((count + 1) as f64).ln() + log_probability
            - log_failure;
        log_total = log_add_exp(log_total, log_term);
        count += 1;
    }
    log_total.exp().min(1.0)
}

fn binomial_upper_tail(successes: u64, trials: u64, probability: f64) -> f64 {
    if probability <= 0.0 {
        return 0.0;
    }
    if probability >= 1.0 || successes == 0 {
        return 1.0;
    }
    let log_probability = probability.ln();
    let log_failure = (-probability).ln_1p();
    let mut log_term = trials as f64 * log_probability;
    let mut log_total = log_term;
    let mut count = trials;
    while count > successes {
        log_term += (count as f64).ln() - ((trials - count + 1) as f64).ln() + log_failure
            - log_probability;
        log_total = log_add_exp(log_total, log_term);
        count -= 1;
    }
    log_total.exp().min(1.0)
}

fn log_add_exp(left: f64, right: f64) -> f64 {
    let maximum = left.max(right);
    maximum + (left.min(right) - maximum).exp().ln_1p()
}

fn mean(values: &[f64]) -> f64 {
    values.iter().sum::<f64>() / values.len() as f64
}

fn percentile_f64(values: &[f64], percentile: usize) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let rank = (percentile * values.len()).div_ceil(100).saturating_sub(1);
    Some(values[rank])
}

fn percentile_sorted_i64(values: &[i64], percentile: usize) -> Option<i64> {
    if values.is_empty() {
        return None;
    }
    let rank = (percentile * values.len()).div_ceil(100).saturating_sub(1);
    Some(values[rank])
}

/// Exact one-sided distribution-free upper bound for a population quantile.
/// The returned order statistic has at least `1 - alpha` coverage. A short
/// sample can have no finite bound, which the gate reports as non-passing.
fn quantile_upper_confidence_bound(
    sorted_values: &[i64],
    percentile: u64,
    alpha: f64,
) -> Option<i64> {
    if sorted_values.is_empty() {
        return None;
    }
    let trials = u64::try_from(sorted_values.len()).expect("latency count fits u64");
    let probability = percentile as f64 / 100.0;
    let target_coverage = 1.0 - alpha;
    let mut low = 0_u64;
    let mut high = trials;
    while low < high {
        let index = low + (high - low) / 2;
        if binomial_cdf(index, trials, probability) >= target_coverage {
            high = index;
        } else {
            low = index + 1;
        }
    }
    (low < trials).then(|| {
        sorted_values[usize::try_from(low).expect("latency order-statistic index fits usize")]
    })
}
