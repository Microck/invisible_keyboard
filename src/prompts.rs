//! Project-authored deterministic calibration prompt corpus and selection.

use std::{error::Error, fmt};

use serde::{Deserialize, Serialize};

use crate::{
    layout::{
        Hand, KeyUsage, PHYSICAL_POSITION_COUNT, PhysicalPosition, all_slots,
        keyboard_warm_start_eligible, position_for_label, position_for_usage, position_index,
    },
    manifest::{format_lower_hex, format_sha256, parse_sha256, sha256},
    pipeline::REARM_IDLE_WINDOWS,
};

/// Frozen prompt-corpus identity stored in acquisition and model artifacts.
pub const PROMPT_CORPUS_VERSION: &str = "invisible-keyboard-prompts-v1";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PromptKind {
    Natural,
    Coverage,
    Reserved,
}

/// Stable prompt identity that callers can compare and persist but not forge.
#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct PromptId(String);

impl PromptId {
    fn trusted(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for PromptId {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for PromptId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// One rendered instruction and its exact expected physical labels.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Prompt {
    pub id: PromptId,
    pub kind: PromptKind,
    pub instruction: &'static str,
    pub expected_positions: Vec<PhysicalPosition>,
}

#[derive(Clone, Copy)]
struct PromptDefinition {
    id: &'static str,
    kind: PromptKind,
    instruction: &'static str,
    sequence: &'static str,
}

const DEFINITIONS: &[PromptDefinition] = &[
    PromptDefinition {
        id: "natural-01",
        kind: PromptKind::Natural,
        instruction: "Type: quiet hands move with light steady taps",
        sequence: "quiet hands move with light steady taps",
    },
    PromptDefinition {
        id: "natural-02",
        kind: PromptKind::Natural,
        instruction: "Type: calm practice builds a clear rhythm",
        sequence: "calm practice builds a clear rhythm",
    },
    PromptDefinition {
        id: "natural-03",
        kind: PromptKind::Natural,
        instruction: "Type: keep both wrists loose and level",
        sequence: "keep both wrists loose and level",
    },
    PromptDefinition {
        id: "coverage-01",
        kind: PromptKind::Coverage,
        instruction: "Type each letter once: abcdefghijklmnopqrstuvwxyz",
        sequence: "abcdefghijklmnopqrstuvwxyz",
    },
    PromptDefinition {
        id: "coverage-02",
        kind: PromptKind::Coverage,
        instruction: "Type: quiz six brave foxes with gentle jumps",
        sequence: "quiz six brave foxes with gentle jumps",
    },
    PromptDefinition {
        id: "coverage-03",
        kind: PromptKind::Coverage,
        instruction: "Type: pack blue jars next to the woven quilt",
        sequence: "pack blue jars next to the woven quilt",
    },
    PromptDefinition {
        id: "reserved-01",
        kind: PromptKind::Reserved,
        instruction: "Type these four separators: comma period semicolon slash",
        sequence: ",.;/",
    },
];

/// Stateless access to the one version 1 corpus.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PromptCorpus;

impl PromptCorpus {
    #[must_use]
    pub const fn v1() -> Self {
        Self
    }

    #[must_use]
    pub const fn version(self) -> &'static str {
        PROMPT_CORPUS_VERSION
    }

    /// Render every frozen prompt and its expected position sequence.
    #[must_use]
    pub fn prompts(self) -> Vec<Prompt> {
        DEFINITIONS
            .iter()
            .map(|definition| Prompt {
                id: PromptId::trusted(definition.id),
                kind: definition.kind,
                instruction: definition.instruction,
                expected_positions: parse_sequence(definition.sequence)
                    .into_iter()
                    .filter(|position| keyboard_warm_start_eligible(*position))
                    .collect(),
            })
            .collect()
    }

    /// Select a prompt by current per-position deficits and attempt number.
    ///
    /// Selection never reads user text and uses no language model. Equal-score
    /// prompts rotate deterministically so a stalled key is shown in different
    /// neighbors without an infinite random loop.
    #[must_use]
    pub fn next_keyboard_prompt(
        self,
        deficits: &[u16; PHYSICAL_POSITION_COUNT],
        attempt: u32,
    ) -> Prompt {
        let prompts = self.prompts();
        let scores = prompts
            .iter()
            .map(|prompt| prompt_score(prompt, deficits))
            .collect::<Vec<_>>();
        let best_score = scores.iter().copied().max().unwrap_or(0);
        let candidates = scores
            .iter()
            .enumerate()
            .filter_map(|(index, &score)| (score == best_score).then_some(index))
            .collect::<Vec<_>>();
        let candidate_index = usize::try_from(attempt).unwrap_or(usize::MAX) % candidates.len();
        prompts[candidates[candidate_index]].clone()
    }

    /// Canonical ASCII bytes used for the corpus identity.
    #[must_use]
    pub fn canonical_bytes(self) -> Vec<u8> {
        let mut output = format!("version|{PROMPT_CORPUS_VERSION}\n");
        for definition in DEFINITIONS {
            output.push_str(definition.id);
            output.push('|');
            output.push_str(kind_name(definition.kind));
            output.push('|');
            output.push_str(definition.instruction);
            output.push('|');
            output.push_str(definition.sequence);
            output.push('\n');
        }
        output.into_bytes()
    }

    #[must_use]
    pub fn sha256(self) -> [u8; 32] {
        sha256(&self.canonical_bytes())
    }
}

fn prompt_score(prompt: &Prompt, deficits: &[u16; PHYSICAL_POSITION_COUNT]) -> u32 {
    prompt
        .expected_positions
        .iter()
        .map(|position| {
            let index =
                position_index(*position).expect("frozen prompt position belongs to the layout");
            u32::from(deficits[index])
        })
        .sum()
}

fn parse_sequence(sequence: &str) -> Vec<PhysicalPosition> {
    let mut positions = Vec::new();
    let mut remaining = sequence;
    while let Some(character) = remaining.chars().next() {
        remaining = &remaining[character.len_utf8()..];
        let position = match character {
            'a'..='z' => position_for_usage(
                KeyUsage::from_ascii_letter(character).expect("ASCII letter maps to Core-28"),
            ),
            ' ' => position_for_usage(KeyUsage::Space),
            ',' => position_for_label("comma"),
            '.' => position_for_label("period"),
            ';' => position_for_label("semicolon"),
            '/' => position_for_label("slash"),
            _ => None,
        }
        .unwrap_or_else(|| panic!("unsupported frozen prompt character {character:?}"));
        positions.push(position);
    }
    positions
}

const fn kind_name(kind: PromptKind) -> &'static str {
    match kind {
        PromptKind::Natural => "natural",
        PromptKind::Coverage => "coverage",
        PromptKind::Reserved => "reserved",
    }
}

/// Version 1 passive-surface training-plan discriminator.
pub const SURFACE_TRAINING_PLAN_SCHEMA: &str = "invisible-keyboard-surface-training-plan-v1";

/// Version 1 passive-surface held-out-plan discriminator.
pub const HELD_OUT_SURFACE_PLAN_SCHEMA: &str = "invisible-keyboard-held-out-surface-plan-v1";

/// Maximum accepted canonical plan size before JSON parsing.
pub const MAX_SURFACE_PLAN_BYTES: usize = 262_144;

/// Fixed per-position dose for both passive-surface plan domains.
pub const SURFACE_POSITION_REPETITIONS: usize = 3;

/// Number of prompted taps in either version 1 surface plan.
pub const SURFACE_TARGET_COUNT: usize = PHYSICAL_POSITION_COUNT * SURFACE_POSITION_REPETITIONS;

/// Disjoint passive taps used only to select the final intent threshold.
pub const PASSIVE_THRESHOLD_TARGET_COUNT: usize = 10;

/// Frozen response window relative to the on-screen cue.
pub const SURFACE_RESPONSE_WINDOW_START_NS: i64 = 0;

/// Provisional finite response window relative to the rendered on-screen cue.
/// Hardware trials must confirm or replace this human-response allowance.
pub const SURFACE_RESPONSE_WINDOW_END_NS: i64 = 1_500_000_000;

/// Frozen number of idle decoder windows required after each response window.
pub const SURFACE_REARM_IDLE_WINDOWS: u8 = REARM_IDLE_WINDOWS;

/// The version 1 plan binds rearm timing to the 200 Hz software baseline.
pub const SURFACE_REARM_GUARD_NS: u64 = 20_000_000;

/// Explicit quiet-null interval after each prompted tap.
pub const SURFACE_INTER_PROMPT_NULL_NS: u64 = 400_000_000;

/// Provisional ordinary-motion reserve in the held-out mount check.
pub const HELD_OUT_ORDINARY_MOTION_EXPOSURE_MS: u64 =
    crate::validation::MIN_HELD_OUT_ORDINARY_MOTION_EXPOSURE_MS;

const SURFACE_CUE_OFFSET_NS: u64 = 300_000_000;
// A target remains alone until its complete 1.5-second response gate closes.
const SURFACE_TARGET_SEGMENT_NS: u64 = 1_820_000_000;
// Six seconds of total margin lets 75 ms evidence windows cover the full
// provisional 60-second gate despite cue alignment and interval endpoints.
const HELD_OUT_MOTION_SEGMENT_NS: u64 = 22_000_000_000;

/// Provenance label that prevents training and held-out plans from being
/// interpreted as the same evidence domain.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum SurfacePlanDomain {
    #[serde(rename = "passive-surface-training")]
    Training,
    #[serde(rename = "passive-surface-held-out-validation")]
    HeldOutValidation,
}

/// Capture-facing segment role. Prompted segments deliberately contain no
/// physical-position field; positions live only in scoring metadata.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SurfaceSegmentKind {
    PromptedTap,
    ThresholdTap,
    ThresholdQuiet,
    QuietNull,
    OrdinaryMotion,
}

/// One browser-rendered instruction within a canonical surface segment.
///
/// Every segment starts with a preparation instruction. Tap segments then
/// publish one cue. Both acknowledgements shift the remaining schedule, so
/// capture admission must retain them as separate facts.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SurfacePresentationPhase {
    Preparation,
    Cue,
}

/// One plan-relative browser presentation owned by a surface segment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScheduledSurfacePresentation {
    phase: SurfacePresentationPhase,
    at_ns: u64,
}

impl ScheduledSurfacePresentation {
    #[must_use]
    pub const fn phase(self) -> SurfacePresentationPhase {
        self.phase
    }

    #[must_use]
    pub const fn at_ns(self) -> u64 {
        self.at_ns
    }
}

/// One half-open interval on a plan-relative monotonic timeline.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SurfaceInterval {
    start_ns: u64,
    end_ns: u64,
}

impl SurfaceInterval {
    const fn new(start_ns: u64, end_ns: u64) -> Self {
        Self { start_ns, end_ns }
    }

    #[must_use]
    pub const fn start_ns(self) -> u64 {
        self.start_ns
    }

    #[must_use]
    pub const fn end_ns(self) -> u64 {
        self.end_ns
    }

    #[must_use]
    pub const fn duration_ns(self) -> u64 {
        self.end_ns - self.start_ns
    }

    #[must_use]
    pub const fn overlaps(self, other: Self) -> bool {
        self.start_ns < other.end_ns && other.start_ns < self.end_ns
    }
}

/// Frozen timing values copied into each canonical surface plan.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SurfaceTimingContract {
    response_window_start_ns: i64,
    response_window_end_ns: i64,
    rearm_idle_windows: u8,
    rearm_guard_ns: u64,
}

impl SurfaceTimingContract {
    const fn v1() -> Self {
        Self {
            response_window_start_ns: SURFACE_RESPONSE_WINDOW_START_NS,
            response_window_end_ns: SURFACE_RESPONSE_WINDOW_END_NS,
            rearm_idle_windows: SURFACE_REARM_IDLE_WINDOWS,
            rearm_guard_ns: SURFACE_REARM_GUARD_NS,
        }
    }

    #[must_use]
    pub const fn response_window_start_ns(self) -> i64 {
        self.response_window_start_ns
    }

    #[must_use]
    pub const fn response_window_end_ns(self) -> i64 {
        self.response_window_end_ns
    }

    #[must_use]
    pub const fn rearm_idle_windows(self) -> u8 {
        self.rearm_idle_windows
    }

    #[must_use]
    pub const fn rearm_guard_ns(self) -> u64 {
        self.rearm_guard_ns
    }
}

/// One immutable capture/presentation segment.
///
/// A contact detector may consume this type because it carries only timing,
/// a prompt join ID, and an optional hand for scripted ordinary motion. It
/// does not carry a prompted tap's truth position or assert that contact
/// occurred.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SurfacePromptSegment {
    id: PromptId,
    kind: SurfaceSegmentKind,
    start_ns: u64,
    end_ns: u64,
    cue_at_ns: Option<u64>,
    motion_hand: Option<Hand>,
}

impl SurfacePromptSegment {
    #[must_use]
    pub const fn id(&self) -> &PromptId {
        &self.id
    }

    #[must_use]
    pub const fn kind(&self) -> SurfaceSegmentKind {
        self.kind
    }

    #[must_use]
    pub const fn start_ns(&self) -> u64 {
        self.start_ns
    }

    #[must_use]
    pub const fn end_ns(&self) -> u64 {
        self.end_ns
    }

    #[must_use]
    pub const fn duration_ns(&self) -> u64 {
        self.end_ns - self.start_ns
    }

    #[must_use]
    pub const fn interval(&self) -> SurfaceInterval {
        SurfaceInterval::new(self.start_ns, self.end_ns)
    }

    #[must_use]
    pub const fn cue_at_ns(&self) -> Option<u64> {
        self.cue_at_ns
    }

    #[must_use]
    pub const fn motion_hand(&self) -> Option<Hand> {
        self.motion_hand
    }

    /// Return every browser presentation that can shift this segment's
    /// effective host-time schedule, in canonical publication order.
    pub fn scheduled_presentations(&self) -> impl Iterator<Item = ScheduledSurfacePresentation> {
        std::iter::once(ScheduledSurfacePresentation {
            phase: SurfacePresentationPhase::Preparation,
            at_ns: self.start_ns,
        })
        .chain(self.cue_at_ns.map(|at_ns| ScheduledSurfacePresentation {
            phase: SurfacePresentationPhase::Cue,
            at_ns,
        }))
    }

    #[must_use]
    pub fn response_interval(&self) -> Option<SurfaceInterval> {
        let cue_at_ns = self.cue_at_ns?;
        let start_ns = cue_at_ns.checked_add_signed(SURFACE_RESPONSE_WINDOW_START_NS)?;
        let end_ns = cue_at_ns.checked_add_signed(SURFACE_RESPONSE_WINDOW_END_NS)?;
        Some(SurfaceInterval::new(start_ns, end_ns))
    }

    #[must_use]
    pub fn rearm_interval(&self) -> Option<SurfaceInterval> {
        let response = self.response_interval()?;
        Some(SurfaceInterval::new(
            response.end_ns(),
            response.end_ns().checked_add(SURFACE_REARM_GUARD_NS)?,
        ))
    }
}

/// Position truth kept outside capture-facing segments.
///
/// This value is join/scoring metadata. Prompt identity is not contact truth,
/// and callers must not synthesize a detected contact from this table.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SurfaceTargetMetadata {
    prompt_id: PromptId,
    target_ordinal: usize,
    position: PhysicalPosition,
}

impl SurfaceTargetMetadata {
    #[must_use]
    pub const fn prompt_id(&self) -> &PromptId {
        &self.prompt_id
    }

    #[must_use]
    pub const fn target_ordinal(&self) -> usize {
        self.target_ordinal
    }

    #[must_use]
    pub const fn position(&self) -> PhysicalPosition {
        self.position
    }
}

/// Deterministic, non-adaptive passive-surface fitting plan.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SurfaceTrainingPlan {
    schema: String,
    domain: SurfacePlanDomain,
    timing: SurfaceTimingContract,
    position_repetitions: usize,
    segments: Vec<SurfacePromptSegment>,
    scoring_targets: Vec<SurfaceTargetMetadata>,
}

impl SurfaceTrainingPlan {
    /// Build the only version 1 training plan.
    #[must_use]
    pub fn v1() -> Self {
        let mut schedule = SurfaceScheduleBuilder::new("surface-training-v1".to_owned());
        for _ in 0..SURFACE_POSITION_REPETITIONS {
            for slot in all_slots() {
                schedule.push_target(slot.position);
            }
        }
        for hand in [Hand::Left, Hand::Right] {
            for finger in [
                crate::layout::Finger::Thumb,
                crate::layout::Finger::Index,
                crate::layout::Finger::Middle,
                crate::layout::Finger::Ring,
                crate::layout::Finger::Pinky,
            ] {
                let position = all_slots()
                    .iter()
                    .find(|slot| slot.position.hand == hand && slot.position.finger == finger)
                    .expect("every hand/finger has a passive threshold target")
                    .position;
                schedule.push_threshold_target(position);
            }
            for _ in 0..3 {
                schedule.push_threshold_quiet(hand);
            }
        }
        let (segments, scoring_targets) = schedule.finish();
        Self {
            schema: SURFACE_TRAINING_PLAN_SCHEMA.to_owned(),
            domain: SurfacePlanDomain::Training,
            timing: SurfaceTimingContract::v1(),
            position_repetitions: SURFACE_POSITION_REPETITIONS,
            segments,
            scoring_targets,
        }
    }

    /// Parse only the byte-for-byte semantic version 1 plan.
    ///
    /// # Errors
    ///
    /// Rejects oversized, malformed, unknown-field, reordered, or mutated
    /// plans. A caller cannot use parsing to create an adaptive variant.
    pub fn from_json(source: &[u8]) -> Result<Self, SurfacePlanError> {
        require_plan_size(source)?;
        let plan = serde_json::from_slice::<Self>(source)
            .map_err(|error| SurfacePlanError::Json(error.to_string()))?;
        plan.validate()?;
        Ok(plan)
    }

    pub fn validate(&self) -> Result<(), SurfacePlanError> {
        if *self != Self::v1() {
            return Err(SurfacePlanError::NonCanonicalTrainingPlan);
        }
        Ok(())
    }

    #[must_use]
    pub const fn domain(&self) -> SurfacePlanDomain {
        self.domain
    }

    #[must_use]
    pub const fn timing(&self) -> SurfaceTimingContract {
        self.timing
    }

    #[must_use]
    pub fn segments(&self) -> &[SurfacePromptSegment] {
        &self.segments
    }

    /// Return scoring/join metadata separately from capture-facing segments.
    #[must_use]
    pub fn scoring_targets(&self) -> &[SurfaceTargetMetadata] {
        &self.scoring_targets
    }

    #[must_use]
    pub fn duration_ns(&self) -> u64 {
        self.segments.last().map_or(0, SurfacePromptSegment::end_ns)
    }

    pub fn canonical_json(&self) -> Result<Vec<u8>, SurfacePlanError> {
        self.validate()?;
        serde_json::to_vec(self).map_err(|error| SurfacePlanError::Serialization(error.to_string()))
    }

    pub fn canonical_bytes(&self) -> Result<Vec<u8>, SurfacePlanError> {
        self.canonical_json()
    }

    pub fn sha256(&self) -> Result<[u8; 32], SurfacePlanError> {
        Ok(sha256(&self.canonical_bytes()?))
    }
}

/// Artifact-bound non-adaptive passive-surface validation plan.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HeldOutSurfacePlan {
    schema: String,
    domain: SurfacePlanDomain,
    locked_artifact_sha256: String,
    run_seed: u64,
    timing: SurfaceTimingContract,
    position_repetitions: usize,
    required_ordinary_motion_exposure_ms: u64,
    segments: Vec<SurfacePromptSegment>,
    scoring_targets: Vec<SurfaceTargetMetadata>,
}

impl HeldOutSurfacePlan {
    /// Derive a frozen unseen sequence from a locked model identity and a run
    /// seed. There is no constructor that accepts targets or mutable state.
    #[must_use]
    pub fn v1(locked_artifact_sha256: [u8; 32], run_seed: u64) -> Self {
        let prefix = held_out_prompt_prefix(&locked_artifact_sha256, run_seed);
        let mut schedule = SurfaceScheduleBuilder::new(prefix);
        for round in 0..SURFACE_POSITION_REPETITIONS {
            for position in held_out_round(&locked_artifact_sha256, run_seed, round) {
                schedule.push_target(position);
            }
            schedule.push_ordinary_motion(Hand::Left, HELD_OUT_MOTION_SEGMENT_NS);
            schedule.push_ordinary_motion(Hand::Right, HELD_OUT_MOTION_SEGMENT_NS);
        }
        let (segments, scoring_targets) = schedule.finish();
        Self {
            schema: HELD_OUT_SURFACE_PLAN_SCHEMA.to_owned(),
            domain: SurfacePlanDomain::HeldOutValidation,
            locked_artifact_sha256: format_sha256(&locked_artifact_sha256),
            run_seed,
            timing: SurfaceTimingContract::v1(),
            position_repetitions: SURFACE_POSITION_REPETITIONS,
            required_ordinary_motion_exposure_ms: HELD_OUT_ORDINARY_MOTION_EXPOSURE_MS,
            segments,
            scoring_targets,
        }
    }

    /// Parse a plan only in the context of its expected locked artifact and
    /// run. Supplying JSON alone is intentionally insufficient.
    ///
    /// # Errors
    ///
    /// Rejects oversized, malformed, unknown-field, wrong-binding, reordered,
    /// overlapping, or otherwise mutated plans.
    pub fn from_json(
        source: &[u8],
        locked_artifact_sha256: [u8; 32],
        run_seed: u64,
    ) -> Result<Self, SurfacePlanError> {
        require_plan_size(source)?;
        let plan = serde_json::from_slice::<Self>(source)
            .map_err(|error| SurfacePlanError::Json(error.to_string()))?;
        if plan.locked_artifact_sha256 != format_sha256(&locked_artifact_sha256) {
            return Err(SurfacePlanError::ArtifactBindingMismatch);
        }
        if plan.run_seed != run_seed {
            return Err(SurfacePlanError::RunSeedBindingMismatch);
        }
        plan.validate()?;
        Ok(plan)
    }

    pub fn validate(&self) -> Result<(), SurfacePlanError> {
        let artifact_sha256 = parse_sha256(&self.locked_artifact_sha256)
            .ok_or(SurfacePlanError::InvalidArtifactSha256)?;
        if *self != Self::v1(artifact_sha256, self.run_seed) {
            return Err(SurfacePlanError::NonCanonicalHeldOutPlan);
        }
        Ok(())
    }

    #[must_use]
    pub const fn domain(&self) -> SurfacePlanDomain {
        self.domain
    }

    #[must_use]
    pub fn locked_artifact_sha256(&self) -> [u8; 32] {
        parse_sha256(&self.locked_artifact_sha256)
            .expect("constructed held-out plan contains a typed SHA-256")
    }

    #[must_use]
    pub const fn run_seed(&self) -> u64 {
        self.run_seed
    }

    #[must_use]
    pub const fn timing(&self) -> SurfaceTimingContract {
        self.timing
    }

    #[must_use]
    pub fn segments(&self) -> &[SurfacePromptSegment] {
        &self.segments
    }

    /// Return scoring/join metadata separately from capture-facing segments.
    #[must_use]
    pub fn scoring_targets(&self) -> &[SurfaceTargetMetadata] {
        &self.scoring_targets
    }

    #[must_use]
    pub fn duration_ns(&self) -> u64 {
        self.segments.last().map_or(0, SurfacePromptSegment::end_ns)
    }

    /// Derive exposure from explicit immutable intervals, never a caller count.
    #[must_use]
    pub fn ordinary_motion_exposure_ms(&self) -> u64 {
        [Hand::Left, Hand::Right]
            .into_iter()
            .map(|hand| {
                exposure_for_hand_ns(&self.segments, hand, |kind| {
                    kind == SurfaceSegmentKind::OrdinaryMotion
                })
            })
            .min()
            .unwrap_or(0)
            / 1_000_000
    }

    /// Quiet rest and scripted motion both belong to the null truth row.
    #[must_use]
    pub fn null_exposure_ms(&self) -> u64 {
        [Hand::Left, Hand::Right]
            .into_iter()
            .map(|hand| {
                exposure_for_hand_ns(&self.segments, hand, |kind| {
                    kind == SurfaceSegmentKind::QuietNull
                        || kind == SurfaceSegmentKind::OrdinaryMotion
                })
            })
            .min()
            .unwrap_or(0)
            / 1_000_000
    }

    pub fn canonical_json(&self) -> Result<Vec<u8>, SurfacePlanError> {
        self.validate()?;
        serde_json::to_vec(self).map_err(|error| SurfacePlanError::Serialization(error.to_string()))
    }

    pub fn canonical_bytes(&self) -> Result<Vec<u8>, SurfacePlanError> {
        self.canonical_json()
    }

    pub fn sha256(&self) -> Result<[u8; 32], SurfacePlanError> {
        Ok(sha256(&self.canonical_bytes()?))
    }
}

#[derive(Clone, Debug)]
struct SurfaceScheduleBuilder {
    prompt_prefix: String,
    next_segment_ordinal: usize,
    cursor_ns: u64,
    segments: Vec<SurfacePromptSegment>,
    scoring_targets: Vec<SurfaceTargetMetadata>,
}

impl SurfaceScheduleBuilder {
    fn new(prompt_prefix: String) -> Self {
        Self {
            prompt_prefix,
            next_segment_ordinal: 0,
            cursor_ns: 0,
            segments: Vec::with_capacity(
                (SURFACE_TARGET_COUNT + PASSIVE_THRESHOLD_TARGET_COUNT) * 2 + 12,
            ),
            scoring_targets: Vec::with_capacity(
                SURFACE_TARGET_COUNT + PASSIVE_THRESHOLD_TARGET_COUNT,
            ),
        }
    }

    fn next_id(&mut self) -> PromptId {
        let id = PromptId::trusted(format!(
            "{}-{:04}",
            self.prompt_prefix, self.next_segment_ordinal
        ));
        self.next_segment_ordinal += 1;
        id
    }

    fn push_target(&mut self, position: PhysicalPosition) {
        self.push_tap(position, SurfaceSegmentKind::PromptedTap);
    }

    fn push_threshold_target(&mut self, position: PhysicalPosition) {
        self.push_tap(position, SurfaceSegmentKind::ThresholdTap);
    }

    fn push_tap(&mut self, position: PhysicalPosition, kind: SurfaceSegmentKind) {
        let id = self.next_id();
        let start_ns = self.cursor_ns;
        let end_ns = start_ns + SURFACE_TARGET_SEGMENT_NS;
        self.segments.push(SurfacePromptSegment {
            id: id.clone(),
            kind,
            start_ns,
            end_ns,
            cue_at_ns: Some(start_ns + SURFACE_CUE_OFFSET_NS),
            motion_hand: None,
        });
        self.scoring_targets.push(SurfaceTargetMetadata {
            prompt_id: id,
            target_ordinal: self.scoring_targets.len(),
            position,
        });
        self.cursor_ns = end_ns;
        self.push_null(SURFACE_INTER_PROMPT_NULL_NS);
    }

    fn push_threshold_quiet(&mut self, hand: Hand) {
        let id = self.next_id();
        let start_ns = self.cursor_ns;
        let end_ns = start_ns + SURFACE_INTER_PROMPT_NULL_NS;
        self.segments.push(SurfacePromptSegment {
            id,
            kind: SurfaceSegmentKind::ThresholdQuiet,
            start_ns,
            end_ns,
            cue_at_ns: None,
            motion_hand: Some(hand),
        });
        self.cursor_ns = end_ns;
    }

    fn push_null(&mut self, duration_ns: u64) {
        let id = self.next_id();
        let start_ns = self.cursor_ns;
        let end_ns = start_ns + duration_ns;
        self.segments.push(SurfacePromptSegment {
            id,
            kind: SurfaceSegmentKind::QuietNull,
            start_ns,
            end_ns,
            cue_at_ns: None,
            motion_hand: None,
        });
        self.cursor_ns = end_ns;
    }

    fn push_ordinary_motion(&mut self, hand: Hand, duration_ns: u64) {
        let id = self.next_id();
        let start_ns = self.cursor_ns;
        let end_ns = start_ns + duration_ns;
        self.segments.push(SurfacePromptSegment {
            id,
            kind: SurfaceSegmentKind::OrdinaryMotion,
            start_ns,
            end_ns,
            cue_at_ns: None,
            motion_hand: Some(hand),
        });
        self.cursor_ns = end_ns;
    }

    fn finish(self) -> (Vec<SurfacePromptSegment>, Vec<SurfaceTargetMetadata>) {
        (self.segments, self.scoring_targets)
    }
}

fn held_out_prompt_prefix(artifact_sha256: &[u8; 32], run_seed: u64) -> String {
    let digest = held_out_digest(artifact_sha256, run_seed, usize::MAX, usize::MAX);
    let tag = format_lower_hex(&digest[..8]);
    format!("surface-held-out-v1-{tag}")
}

fn held_out_round(
    artifact_sha256: &[u8; 32],
    run_seed: u64,
    round: usize,
) -> Vec<PhysicalPosition> {
    let mut left = all_slots()
        .iter()
        .filter(|slot| slot.position.hand == Hand::Left)
        .map(|slot| slot.position)
        .collect::<Vec<_>>();
    let mut right = all_slots()
        .iter()
        .filter(|slot| slot.position.hand == Hand::Right)
        .map(|slot| slot.position)
        .collect::<Vec<_>>();
    left.sort_by_key(|position| {
        held_out_position_rank(artifact_sha256, run_seed, round, *position)
    });
    right.sort_by_key(|position| {
        held_out_position_rank(artifact_sha256, run_seed, round, *position)
    });

    let starts_left = held_out_digest(artifact_sha256, run_seed, round, usize::MAX)[0] & 1 == 0;
    let mut ordered = Vec::with_capacity(PHYSICAL_POSITION_COUNT);
    for index in 0..left.len() {
        if starts_left {
            ordered.push(left[index]);
            ordered.push(right[index]);
        } else {
            ordered.push(right[index]);
            ordered.push(left[index]);
        }
    }
    ordered
}

fn held_out_position_rank(
    artifact_sha256: &[u8; 32],
    run_seed: u64,
    round: usize,
    position: PhysicalPosition,
) -> [u8; 32] {
    let index = position_index(position).expect("held-out position belongs to the layout");
    held_out_digest(artifact_sha256, run_seed, round, index)
}

fn held_out_digest(
    artifact_sha256: &[u8; 32],
    run_seed: u64,
    round: usize,
    position_index: usize,
) -> [u8; 32] {
    let mut input = Vec::with_capacity(96);
    input.extend_from_slice(HELD_OUT_SURFACE_PLAN_SCHEMA.as_bytes());
    input.push(0);
    input.extend_from_slice(artifact_sha256);
    input.extend_from_slice(&run_seed.to_le_bytes());
    input.extend_from_slice(&round.to_le_bytes());
    input.extend_from_slice(&position_index.to_le_bytes());
    sha256(&input)
}

fn exposure_for_hand_ns(
    segments: &[SurfacePromptSegment],
    hand: Hand,
    mut include: impl FnMut(SurfaceSegmentKind) -> bool,
) -> u64 {
    segments
        .iter()
        .filter(|segment| {
            include(segment.kind())
                && segment
                    .motion_hand()
                    .is_none_or(|motion_hand| motion_hand == hand)
        })
        .map(SurfacePromptSegment::duration_ns)
        .sum()
}

fn require_plan_size(source: &[u8]) -> Result<(), SurfacePlanError> {
    if source.len() > MAX_SURFACE_PLAN_BYTES {
        return Err(SurfacePlanError::TooLarge {
            actual: source.len(),
        });
    }
    Ok(())
}

/// Strict parse, binding, and canonicalization failure for a surface plan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SurfacePlanError {
    TooLarge { actual: usize },
    Json(String),
    Serialization(String),
    InvalidArtifactSha256,
    ArtifactBindingMismatch,
    RunSeedBindingMismatch,
    NonCanonicalTrainingPlan,
    NonCanonicalHeldOutPlan,
}

impl fmt::Display for SurfacePlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge { actual } => write!(
                formatter,
                "surface plan is {actual} bytes; maximum is {MAX_SURFACE_PLAN_BYTES}"
            ),
            Self::Json(error) => write!(formatter, "surface plan JSON is invalid: {error}"),
            Self::Serialization(error) => {
                write!(formatter, "surface plan serialization failed: {error}")
            }
            Self::InvalidArtifactSha256 => {
                formatter.write_str("held-out plan artifact SHA-256 is invalid")
            }
            Self::ArtifactBindingMismatch => {
                formatter.write_str("held-out plan belongs to a different locked artifact")
            }
            Self::RunSeedBindingMismatch => {
                formatter.write_str("held-out plan belongs to a different run seed")
            }
            Self::NonCanonicalTrainingPlan => {
                formatter.write_str("surface training plan is not the frozen version 1 plan")
            }
            Self::NonCanonicalHeldOutPlan => {
                formatter.write_str("held-out surface plan is not its frozen derived plan")
            }
        }
    }
}

impl Error for SurfacePlanError {}
