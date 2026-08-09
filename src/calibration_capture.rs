//! Local capture journal and deterministic keyboard/sensor window matching.
//!
//! The live adapter may collect keyboard and sensor events concurrently. This
//! module keeps the I/O out of the model fitter: it converts one immutable raw
//! journal plus one strict sensor session into the smaller calibration
//! evidence sidecar.

use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt,
};

use serde::{Deserialize, Serialize};

use crate::{
    artifact::KeyboardIdentity,
    calibration_engine::{
        CalibrationEvidence, CalibrationEvidenceError, CalibrationEvidenceInput,
        CalibrationObservation, EvidenceDomain, EvidenceNullKind, EvidencePurpose, EvidenceRole,
    },
    clock_epoch::{ClockEpochError, ClockEpochFitError, ClockEpochSet, QualifiedOfflineClocks},
    decoder::{
        AbstainReason, FrameIntegrity, FrameIntegrityError, HandDecoder, TapWindow, WindowError,
    },
    labels::{
        INTENT_WINDOW_END_NS, INTENT_WINDOW_START_NS, KeyboardLabelFilter, LabelDisposition,
        RawKeyState, RawKeyboardEvent,
    },
    layout::{Finger, Hand, PhysicalPosition, keyboard_warm_start_eligible},
    manifest::{
        AcquisitionManifest, FrameCanonicalizationError, FrameCanonicalizer, ManifestError,
        ObservedTopologyError, format_sha256, parse_sha256, sha256,
    },
    neutral::NeutralRecordWindow,
    prompts::{
        HeldOutSurfacePlan, PromptId, SurfacePlanError, SurfacePresentationPhase,
        SurfacePromptSegment, SurfaceSegmentKind, SurfaceTargetMetadata, SurfaceTrainingPlan,
    },
    session::{FinalizedSession, SessionError, SessionRecordKind},
};

pub const CALIBRATION_CAPTURE_SCHEMA: &str = "invisible-keyboard-calibration-capture-v9";
pub const CALIBRATION_MATCHING_RULE_SCHEMA: &str = "invisible-keyboard-causal-contact-rule-v1";
pub const MAX_CALIBRATION_CAPTURE_BYTES: usize = 16_777_216;
pub const MAX_CALIBRATION_MATCHING_RULE_BYTES: usize = 4_096;

pub const CONTACT_FEATURE_SCHEMA: &str = "dynamic-finger-to-dorsal-energy-v1";
pub const CONTACT_DEBOUNCE_WINDOWS: u8 = 2;
pub const CONTACT_REARM_WINDOWS: u8 = 4;
pub const CONTACT_RESPONSE_START_NS: u64 = 0;
/// Provisional human response allowance after the browser render receipt.
/// It is finite and deliberately wider than a simple visual reaction time.
pub const CONTACT_RESPONSE_END_NS: u64 = 1_500_000_000;
// These fixed neutral multiples and floors are engineering priors, not
// hardware-validated optima. Physical trials must change them by a rule
// schema revision, never by choosing a per-user policy after held-out data.
const CONTACT_LOW_ENERGY_FLOOR: u64 = 1_024;
const CONTACT_HIGH_ENERGY_FLOOR: u64 = 4_096;
const MOTION_ENERGY_FLOOR: u64 = 4_096;

/// Neutral-derived thresholds for one hand in canonical order.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HandContactThresholds {
    pub hand: Hand,
    pub low_energy: u64,
    pub high_energy: u64,
    pub motion_energy: u64,
}

/// Frozen causal contact rule derived from immutable neutral evidence.
///
/// The browser receipt is absent by design. It opens an association window,
/// but only this sensor rule can emit a contact event inside that window.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CalibrationMatchingRule {
    pub schema: String,
    pub acquisition_manifest_sha256: String,
    pub neutral_windows_sha256: String,
    pub neutral_source_sha256: String,
    pub feature_schema: String,
    pub thresholds: [HandContactThresholds; 2],
    pub debounce_windows: u8,
    pub rearm_windows: u8,
    pub response_start_ns: u64,
    pub response_end_ns: u64,
}

impl CalibrationMatchingRule {
    pub fn from_json(source: &[u8]) -> Result<Self, CalibrationCaptureError> {
        if source.len() > MAX_CALIBRATION_MATCHING_RULE_BYTES {
            return Err(CalibrationCaptureError::MatchingRuleTooLarge {
                actual: source.len(),
            });
        }
        let rule = serde_json::from_slice::<Self>(source)
            .map_err(|error| CalibrationCaptureError::MatchingRuleJson(error.to_string()))?;
        rule.validate()?;
        Ok(rule)
    }

    pub fn validate(&self) -> Result<(), CalibrationCaptureError> {
        if self.schema != CALIBRATION_MATCHING_RULE_SCHEMA {
            return Err(CalibrationCaptureError::InvalidMatchingRuleSchema);
        }
        for (value, field) in [
            (
                &self.acquisition_manifest_sha256,
                "acquisition_manifest_sha256",
            ),
            (&self.neutral_windows_sha256, "neutral_windows_sha256"),
            (&self.neutral_source_sha256, "neutral_source_sha256"),
        ] {
            if parse_sha256(value).is_none() {
                return Err(CalibrationCaptureError::InvalidMatchingRuleSha256(field));
            }
        }
        if self.feature_schema != CONTACT_FEATURE_SCHEMA
            || self.thresholds[0].hand != Hand::Left
            || self.thresholds[1].hand != Hand::Right
            || self.thresholds.iter().any(|thresholds| {
                thresholds.low_energy == 0
                    || thresholds.high_energy <= thresholds.low_energy
                    || thresholds.motion_energy == 0
            })
            || self.debounce_windows != CONTACT_DEBOUNCE_WINDOWS
            || self.rearm_windows != CONTACT_REARM_WINDOWS
            || self.response_start_ns != CONTACT_RESPONSE_START_NS
            || self.response_end_ns != CONTACT_RESPONSE_END_NS
        {
            return Err(CalibrationCaptureError::InvalidMatchingRule);
        }
        Ok(())
    }

    pub fn canonical_json(&self) -> Result<Vec<u8>, CalibrationCaptureError> {
        self.validate()?;
        serde_json::to_vec(self)
            .map_err(|error| CalibrationCaptureError::MatchingRuleSerialization(error.to_string()))
    }

    pub fn fingerprint(&self) -> Result<[u8; 32], CalibrationCaptureError> {
        Ok(sha256(&self.canonical_json()?))
    }
}

/// Browser-render provenance for one plan-owned surface instruction.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SurfacePresentationReceipt {
    planned_host_time_ns: u64,
    published_host_time_ns: u64,
    acknowledged_host_time_ns: u64,
    instruction_revision: u64,
}

impl SurfacePresentationReceipt {
    pub fn new(
        planned_host_time_ns: u64,
        published_host_time_ns: u64,
        acknowledged_host_time_ns: u64,
        instruction_revision: u64,
    ) -> Result<Self, CalibrationCaptureError> {
        let receipt = Self {
            planned_host_time_ns,
            published_host_time_ns,
            acknowledged_host_time_ns,
            instruction_revision,
        };
        receipt.validate()?;
        Ok(receipt)
    }

    fn validate(self) -> Result<(), CalibrationCaptureError> {
        if self.instruction_revision == 0
            || self.published_host_time_ns < self.planned_host_time_ns
            || self.acknowledged_host_time_ns < self.published_host_time_ns
        {
            return Err(CalibrationCaptureError::InvalidSurfacePresentation);
        }
        Ok(())
    }

    #[must_use]
    pub const fn planned_host_time_ns(self) -> u64 {
        self.planned_host_time_ns
    }

    #[must_use]
    pub const fn published_host_time_ns(self) -> u64 {
        self.published_host_time_ns
    }

    #[must_use]
    pub const fn acknowledged_host_time_ns(self) -> u64 {
        self.acknowledged_host_time_ns
    }

    #[must_use]
    pub const fn instruction_revision(self) -> u64 {
        self.instruction_revision
    }
}

/// Exact immutable surface plan that owns every surface-label meaning.
///
/// The journal retains only the parameters needed to rebuild the one canonical
/// plan. A stored hash is checked against that rebuild so a caller cannot pair
/// facts with a different target table.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum CalibrationSurfacePlanBinding {
    Fitting {
        plan_sha256: String,
        started_host_time_ns: u64,
    },
    HeldOutValidation {
        plan_sha256: String,
        started_host_time_ns: u64,
        locked_artifact_sha256: String,
        run_seed: u64,
    },
}

impl CalibrationSurfacePlanBinding {
    pub fn fitting(started_host_time_ns: u64) -> Result<Self, CalibrationCaptureError> {
        let plan_sha256 = SurfaceTrainingPlan::v1()
            .sha256()
            .map_err(CalibrationCaptureError::SurfacePlan)?;
        Ok(Self::Fitting {
            plan_sha256: format_sha256(&plan_sha256),
            started_host_time_ns,
        })
    }

    pub fn held_out(
        plan: &HeldOutSurfacePlan,
        started_host_time_ns: u64,
    ) -> Result<Self, CalibrationCaptureError> {
        plan.validate()
            .map_err(CalibrationCaptureError::SurfacePlan)?;
        Ok(Self::HeldOutValidation {
            plan_sha256: format_sha256(
                &plan
                    .sha256()
                    .map_err(CalibrationCaptureError::SurfacePlan)?,
            ),
            started_host_time_ns,
            locked_artifact_sha256: format_sha256(&plan.locked_artifact_sha256()),
            run_seed: plan.run_seed(),
        })
    }

    #[must_use]
    pub const fn purpose(&self) -> EvidencePurpose {
        match self {
            Self::Fitting { .. } => EvidencePurpose::Fitting,
            Self::HeldOutValidation { .. } => EvidencePurpose::HeldOutValidation,
        }
    }

    #[must_use]
    pub const fn started_host_time_ns(&self) -> u64 {
        match self {
            Self::Fitting {
                started_host_time_ns,
                ..
            }
            | Self::HeldOutValidation {
                started_host_time_ns,
                ..
            } => *started_host_time_ns,
        }
    }

    pub fn canonical_plan_json(&self) -> Result<Vec<u8>, CalibrationCaptureError> {
        match self {
            Self::Fitting { .. } => SurfaceTrainingPlan::v1()
                .canonical_json()
                .map_err(CalibrationCaptureError::SurfacePlan),
            Self::HeldOutValidation {
                locked_artifact_sha256,
                run_seed,
                ..
            } => HeldOutSurfacePlan::v1(
                parse_sha256(locked_artifact_sha256)
                    .ok_or(CalibrationCaptureError::InvalidSurfacePlanSha256)?,
                *run_seed,
            )
            .canonical_json()
            .map_err(CalibrationCaptureError::SurfacePlan),
        }
    }

    pub fn validate_plan_json(&self, source: &[u8]) -> Result<(), CalibrationCaptureError> {
        let parsed = match self {
            Self::Fitting { .. } => SurfaceTrainingPlan::from_json(source)
                .map(|plan| plan.canonical_json())
                .map_err(CalibrationCaptureError::SurfacePlan)?,
            Self::HeldOutValidation {
                locked_artifact_sha256,
                run_seed,
                ..
            } => HeldOutSurfacePlan::from_json(
                source,
                parse_sha256(locked_artifact_sha256)
                    .ok_or(CalibrationCaptureError::InvalidSurfacePlanSha256)?,
                *run_seed,
            )
            .map(|plan| plan.canonical_json())
            .map_err(CalibrationCaptureError::SurfacePlan)?,
        }
        .map_err(CalibrationCaptureError::SurfacePlan)?;
        if parsed != self.canonical_plan_json()? {
            return Err(CalibrationCaptureError::SurfacePlanIdentityMismatch);
        }
        Ok(())
    }

    pub fn validate_locked_artifact(
        &self,
        artifact_sha256: [u8; 32],
    ) -> Result<(), CalibrationCaptureError> {
        match self {
            Self::HeldOutValidation {
                locked_artifact_sha256,
                ..
            } if parse_sha256(locked_artifact_sha256) == Some(artifact_sha256) => Ok(()),
            Self::HeldOutValidation { .. } => {
                Err(CalibrationCaptureError::SurfacePlanArtifactMismatch)
            }
            Self::Fitting { .. } => Err(CalibrationCaptureError::SurfacePlanPurposeMismatch),
        }
    }

    pub(crate) fn validate(&self, purpose: EvidencePurpose) -> Result<(), CalibrationCaptureError> {
        if self.purpose() != purpose {
            return Err(CalibrationCaptureError::SurfacePlanPurposeMismatch);
        }
        let expected_sha256 = sha256(&self.canonical_plan_json()?);
        let stored_sha256 = match self {
            Self::Fitting { plan_sha256, .. } | Self::HeldOutValidation { plan_sha256, .. } => {
                parse_sha256(plan_sha256)
                    .ok_or(CalibrationCaptureError::InvalidSurfacePlanSha256)?
            }
        };
        if stored_sha256 != expected_sha256 {
            return Err(CalibrationCaptureError::SurfacePlanIdentityMismatch);
        }
        Ok(())
    }

    fn planned_ordinary_motion_exposure_ms(&self) -> u64 {
        match self {
            Self::Fitting { .. } => 0,
            Self::HeldOutValidation {
                locked_artifact_sha256,
                run_seed,
                ..
            } => HeldOutSurfacePlan::v1(
                parse_sha256(locked_artifact_sha256)
                    .expect("validated held-out binding contains an artifact hash"),
                *run_seed,
            )
            .ordinary_motion_exposure_ms(),
        }
    }
}

/// One raw input retained until the evidence sidecar has been built.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum CapturedCalibrationEvent {
    Keyboard {
        role: EvidenceRole,
        domain: EvidenceDomain,
        block_id: u32,
        event: RawKeyboardEvent,
    },
    /// A physical-keyboard event retained for key-state and provenance only.
    ///
    /// The capture service uses this for a mistyped prompted key and for every
    /// selected-keyboard event observed during passive capture. The replay
    /// filter must still see each down/up pair, but the event must never become
    /// a keyboard label. Its host-clock interval also excludes overlapping
    /// passive sensor windows below.
    KeyboardStateOnly {
        role: EvidenceRole,
        domain: EvidenceDomain,
        block_id: u32,
        event: RawKeyboardEvent,
    },
    /// One acknowledged plan-owned browser presentation.
    ///
    /// The canonical plan and occurrence order determine whether this is a
    /// preparation, cue, null, or motion instruction. Sensor-derived contact
    /// remains separate and is never asserted by this browser fact.
    DisplayAcknowledged {
        prompt_id: PromptId,
        presentation: SurfacePresentationReceipt,
    },
    Idle {
        prompt_id: PromptId,
        sample_ordinal: u32,
        trigger_host_time_ns: u64,
    },
}

/// Append-only local journal created beside the strict sensor session.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CalibrationCaptureJournal {
    pub schema: String,
    pub purpose: EvidencePurpose,
    pub acquisition_manifest_sha256: String,
    pub clock_epochs_sha256: String,
    pub session_sha256: String,
    pub session_id: [u8; 16],
    pub keyboard_device_id: String,
    pub keyboard: KeyboardIdentity,
    pub setup_seconds: u32,
    pub created_unix_ns: u64,
    pub pre_rejected_examples: u32,
    pub neutral_windows: Vec<NeutralRecordWindow>,
    pub matching_rule: Option<CalibrationMatchingRule>,
    pub surface_plan: Option<CalibrationSurfacePlanBinding>,
    pub events: Vec<CapturedCalibrationEvent>,
}

impl CalibrationCaptureJournal {
    pub fn new(input: CalibrationCaptureInput) -> Result<Self, CalibrationCaptureError> {
        let journal = Self {
            schema: CALIBRATION_CAPTURE_SCHEMA.to_owned(),
            purpose: input.purpose,
            acquisition_manifest_sha256: format_sha256(&input.acquisition_manifest_sha256),
            clock_epochs_sha256: format_sha256(&input.clock_epochs_sha256),
            session_sha256: format_sha256(&input.session_sha256),
            session_id: input.session_id,
            keyboard_device_id: input.keyboard_device_id,
            keyboard: input.keyboard,
            setup_seconds: input.setup_seconds,
            created_unix_ns: input.created_unix_ns,
            pre_rejected_examples: input.pre_rejected_examples,
            neutral_windows: input.neutral_windows,
            matching_rule: input.matching_rule,
            surface_plan: input.surface_plan,
            events: input.events,
        };
        journal.validate()?;
        Ok(journal)
    }

    pub fn from_json(source: &[u8]) -> Result<Self, CalibrationCaptureError> {
        if source.len() > MAX_CALIBRATION_CAPTURE_BYTES {
            return Err(CalibrationCaptureError::TooLarge {
                actual: source.len(),
            });
        }
        let journal = serde_json::from_slice::<Self>(source)
            .map_err(|error| CalibrationCaptureError::Json(error.to_string()))?;
        journal.validate()?;
        Ok(journal)
    }

    pub fn validate(&self) -> Result<(), CalibrationCaptureError> {
        if self.schema != CALIBRATION_CAPTURE_SCHEMA {
            return Err(CalibrationCaptureError::InvalidSchema);
        }
        for (value, field) in [
            (
                &self.acquisition_manifest_sha256,
                "acquisition_manifest_sha256",
            ),
            (&self.clock_epochs_sha256, "clock_epochs_sha256"),
            (&self.session_sha256, "session_sha256"),
        ] {
            if parse_sha256(value).is_none() {
                return Err(CalibrationCaptureError::InvalidSha256(field));
            }
        }
        if self.session_id == [0; 16]
            || self.keyboard_device_id.trim().is_empty()
            || self.events.is_empty()
        {
            return Err(CalibrationCaptureError::IncompleteIdentity);
        }
        match (
            self.purpose,
            &self.matching_rule,
            self.neutral_windows.is_empty(),
        ) {
            (EvidencePurpose::Fitting, rule, false) => {
                if let Some(rule) = rule {
                    rule.validate()?;
                }
            }
            (EvidencePurpose::Fitting, _, true) => {
                return Err(CalibrationCaptureError::MissingNeutralWindows);
            }
            (EvidencePurpose::HeldOutValidation, Some(rule), true) => rule.validate()?,
            (EvidencePurpose::HeldOutValidation, None, true) => {
                return Err(CalibrationCaptureError::MissingMatchingRule);
            }
            (EvidencePurpose::HeldOutValidation, _, false) => {
                return Err(CalibrationCaptureError::UnexpectedNeutralWindows);
            }
        }
        if let Some(surface_plan) = &self.surface_plan {
            surface_plan.validate(self.purpose)?;
        }
        let has_surface_facts = self.events.iter().any(|event| {
            matches!(
                event,
                CapturedCalibrationEvent::DisplayAcknowledged { .. }
                    | CapturedCalibrationEvent::Idle { .. }
            )
        });
        if has_surface_facts && self.surface_plan.is_none() {
            return Err(CalibrationCaptureError::MissingSurfacePlan);
        }
        for event in &self.events {
            match event {
                CapturedCalibrationEvent::Keyboard { role, domain, .. } => {
                    if *domain != EvidenceDomain::Keyboard {
                        return Err(CalibrationCaptureError::KeyboardEventDomainMismatch);
                    }
                    if self.purpose != EvidencePurpose::Fitting || *role != EvidenceRole::Training {
                        return Err(CalibrationCaptureError::KeyboardEventProvenanceMismatch);
                    }
                }
                CapturedCalibrationEvent::KeyboardStateOnly { role, domain, .. } => {
                    if *domain != EvidenceDomain::Keyboard {
                        return Err(CalibrationCaptureError::KeyboardEventDomainMismatch);
                    }
                    let expected_role = match self.purpose {
                        EvidencePurpose::Fitting => EvidenceRole::Training,
                        EvidencePurpose::HeldOutValidation => EvidenceRole::HeldOutValidation,
                    };
                    if *role != expected_role {
                        return Err(CalibrationCaptureError::KeyboardEventProvenanceMismatch);
                    }
                }
                CapturedCalibrationEvent::DisplayAcknowledged { presentation, .. } => {
                    presentation.validate()?;
                }
                CapturedCalibrationEvent::Idle { .. } => {}
            }
        }
        self.validate_surface_facts(false)?;
        Ok(())
    }

    pub fn validate_complete_surface_plan(&self) -> Result<(), CalibrationCaptureError> {
        self.validate()?;
        if self.surface_plan.is_none() {
            return Err(CalibrationCaptureError::MissingSurfacePlan);
        }
        self.validate_surface_facts(true)
    }

    #[must_use]
    pub fn surface_plan(&self) -> Option<&CalibrationSurfacePlanBinding> {
        self.surface_plan.as_ref()
    }

    fn validate_surface_facts(
        &self,
        require_complete: bool,
    ) -> Result<(), CalibrationCaptureError> {
        let Some(binding) = &self.surface_plan else {
            return Ok(());
        };
        let truths = surface_prompt_truth(binding)?;
        let presentations = surface_presentation_truth(binding)?;
        let truth_by_id = truths
            .iter()
            .map(|truth| (truth.prompt_id.as_str(), truth))
            .collect::<BTreeMap<_, _>>();
        let expected_nulls = truths
            .iter()
            .filter(|truth| truth.null_kind.is_some())
            .collect::<Vec<_>>();
        let mut next_presentation = 0_usize;
        let mut next_null = 0_usize;
        let mut cumulative_render_delay_ns = 0_u64;
        let mut actual_segment_intervals = BTreeMap::new();
        let mut active_null: Option<(&str, u32)> = None;
        let mut null_sample_counts = BTreeMap::new();
        let mut last_segment_index = None;
        let mut last_fact_time_ns = None;
        let mut last_presentation_ack_ns = None;
        let mut last_instruction_revision = None;

        for event in &self.events {
            let (prompt_id, fact_time_ns, truth) = match event {
                CapturedCalibrationEvent::DisplayAcknowledged {
                    prompt_id,
                    presentation,
                } => {
                    let truth = truth_by_id
                        .get(prompt_id.as_str())
                        .copied()
                        .ok_or_else(|| {
                            CalibrationCaptureError::UnknownSurfacePrompt(
                                prompt_id.as_str().to_owned(),
                            )
                        })?;
                    let expected = presentations.get(next_presentation).ok_or_else(|| {
                        CalibrationCaptureError::DuplicateSurfacePrompt(
                            prompt_id.as_str().to_owned(),
                        )
                    })?;
                    if expected.prompt_id != *prompt_id {
                        return Err(CalibrationCaptureError::ReorderedSurfacePrompt {
                            expected: expected.prompt_id.as_str().to_owned(),
                            actual: prompt_id.as_str().to_owned(),
                        });
                    }
                    if expected.segment_index != truth.segment_index {
                        return Err(CalibrationCaptureError::SurfacePromptKindMismatch(
                            prompt_id.as_str().to_owned(),
                        ));
                    }
                    let planned_host_time_ns = binding
                        .started_host_time_ns()
                        .checked_add(expected.planned_offset_ns)
                        .and_then(|time| time.checked_add(cumulative_render_delay_ns))
                        .ok_or(CalibrationCaptureError::SurfacePlanTimingOverflow)?;
                    if presentation.planned_host_time_ns() != planned_host_time_ns
                        || last_presentation_ack_ns.is_some_and(|acknowledged| {
                            presentation.published_host_time_ns() < acknowledged
                        })
                    {
                        return Err(CalibrationCaptureError::SurfacePromptTimingMismatch(
                            prompt_id.as_str().to_owned(),
                        ));
                    }
                    if last_instruction_revision
                        .is_some_and(|revision| presentation.instruction_revision() <= revision)
                    {
                        return Err(CalibrationCaptureError::SurfacePromptRevisionMismatch(
                            prompt_id.as_str().to_owned(),
                        ));
                    }
                    let render_delay_ns = presentation
                        .acknowledged_host_time_ns()
                        .checked_sub(planned_host_time_ns)
                        .ok_or_else(|| {
                            CalibrationCaptureError::SurfacePromptTimingMismatch(
                                prompt_id.as_str().to_owned(),
                            )
                        })?;
                    cumulative_render_delay_ns = cumulative_render_delay_ns
                        .checked_add(render_delay_ns)
                        .ok_or(CalibrationCaptureError::SurfacePlanTimingOverflow)?;
                    let previous_start = actual_segment_intervals
                        .get(prompt_id.as_str())
                        .map(|(start, _)| *start);
                    let actual_start_ns = match expected.phase {
                        SurfacePresentationPhase::Preparation => {
                            presentation.acknowledged_host_time_ns()
                        }
                        SurfacePresentationPhase::Cue => previous_start.ok_or_else(|| {
                            CalibrationCaptureError::ReorderedSurfacePrompt {
                                expected: prompt_id.as_str().to_owned(),
                                actual: prompt_id.as_str().to_owned(),
                            }
                        })?,
                    };
                    let actual_end_ns = binding
                        .started_host_time_ns()
                        .checked_add(truth.segment_end_ns)
                        .and_then(|time| time.checked_add(cumulative_render_delay_ns))
                        .ok_or(CalibrationCaptureError::SurfacePlanTimingOverflow)?;
                    if expected.phase == SurfacePresentationPhase::Cue {
                        let response_end_ns = presentation
                            .acknowledged_host_time_ns()
                            .checked_add(CONTACT_RESPONSE_END_NS)
                            .ok_or(CalibrationCaptureError::SurfaceResponseOverflow)?;
                        if response_end_ns > actual_end_ns {
                            return Err(CalibrationCaptureError::SurfacePromptTimingMismatch(
                                prompt_id.as_str().to_owned(),
                            ));
                        }
                    }
                    actual_segment_intervals
                        .insert(prompt_id.as_str(), (actual_start_ns, actual_end_ns));
                    next_presentation += 1;
                    last_presentation_ack_ns = Some(presentation.acknowledged_host_time_ns());
                    last_instruction_revision = Some(presentation.instruction_revision());
                    (prompt_id, presentation.acknowledged_host_time_ns(), truth)
                }
                CapturedCalibrationEvent::Idle {
                    prompt_id,
                    sample_ordinal,
                    trigger_host_time_ns,
                } => {
                    let truth = truth_by_id
                        .get(prompt_id.as_str())
                        .copied()
                        .ok_or_else(|| {
                            CalibrationCaptureError::UnknownSurfacePrompt(
                                prompt_id.as_str().to_owned(),
                            )
                        })?;
                    if truth.null_kind.is_none() {
                        return Err(CalibrationCaptureError::SurfacePromptKindMismatch(
                            prompt_id.as_str().to_owned(),
                        ));
                    }
                    match active_null {
                        Some((active_id, previous_ordinal)) if active_id == prompt_id.as_str() => {
                            if *sample_ordinal != previous_ordinal.saturating_add(1) {
                                return Err(CalibrationCaptureError::SurfaceNullSampleMismatch(
                                    prompt_id.as_str().to_owned(),
                                ));
                            }
                        }
                        _ => {
                            let expected = expected_nulls.get(next_null).ok_or_else(|| {
                                CalibrationCaptureError::DuplicateSurfacePrompt(
                                    prompt_id.as_str().to_owned(),
                                )
                            })?;
                            if expected.prompt_id != *prompt_id || *sample_ordinal != 0 {
                                return Err(CalibrationCaptureError::ReorderedSurfacePrompt {
                                    expected: expected.prompt_id.as_str().to_owned(),
                                    actual: prompt_id.as_str().to_owned(),
                                });
                            }
                            next_null += 1;
                        }
                    }
                    active_null = Some((prompt_id.as_str(), *sample_ordinal));
                    null_sample_counts.insert(prompt_id.as_str(), sample_ordinal.saturating_add(1));
                    let Some((segment_start_ns, segment_end_ns)) =
                        actual_segment_intervals.get(prompt_id.as_str()).copied()
                    else {
                        return Err(CalibrationCaptureError::ReorderedSurfacePrompt {
                            expected: prompt_id.as_str().to_owned(),
                            actual: prompt_id.as_str().to_owned(),
                        });
                    };
                    if *trigger_host_time_ns < segment_start_ns
                        || *trigger_host_time_ns >= segment_end_ns
                    {
                        return Err(CalibrationCaptureError::SurfacePromptTimingMismatch(
                            prompt_id.as_str().to_owned(),
                        ));
                    }
                    (prompt_id, *trigger_host_time_ns, truth)
                }
                CapturedCalibrationEvent::Keyboard { .. }
                | CapturedCalibrationEvent::KeyboardStateOnly { .. } => continue,
            };
            if last_segment_index.is_some_and(|index| truth.segment_index < index) {
                return Err(CalibrationCaptureError::ReorderedSurfacePrompt {
                    expected: "a later canonical plan segment".to_owned(),
                    actual: prompt_id.as_str().to_owned(),
                });
            }
            if last_fact_time_ns.is_some_and(|time| fact_time_ns < time) {
                return Err(CalibrationCaptureError::SurfacePromptTimingMismatch(
                    prompt_id.as_str().to_owned(),
                ));
            }
            last_segment_index = Some(truth.segment_index);
            last_fact_time_ns = Some(fact_time_ns);
        }

        if require_complete {
            if let Some(missing) = presentations.get(next_presentation) {
                return Err(CalibrationCaptureError::MissingSurfacePrompt(
                    missing.prompt_id.as_str().to_owned(),
                ));
            }
            if let Some(missing) = expected_nulls.get(next_null) {
                return Err(CalibrationCaptureError::MissingSurfacePrompt(
                    missing.prompt_id.as_str().to_owned(),
                ));
            }
            for expected in expected_nulls {
                let required_samples = if expected.hand.is_some() { 1 } else { 2 };
                if null_sample_counts
                    .get(expected.prompt_id.as_str())
                    .copied()
                    .unwrap_or(0)
                    < required_samples
                {
                    return Err(CalibrationCaptureError::MissingSurfacePrompt(
                        expected.prompt_id.as_str().to_owned(),
                    ));
                }
            }
        }
        Ok(())
    }

    pub fn canonical_json(&self) -> Result<Vec<u8>, CalibrationCaptureError> {
        self.validate()?;
        serde_json::to_vec(self)
            .map_err(|error| CalibrationCaptureError::Serialization(error.to_string()))
    }
}

pub struct CalibrationCaptureInput {
    pub purpose: EvidencePurpose,
    pub acquisition_manifest_sha256: [u8; 32],
    pub clock_epochs_sha256: [u8; 32],
    pub session_sha256: [u8; 32],
    pub session_id: [u8; 16],
    pub keyboard_device_id: String,
    pub keyboard: KeyboardIdentity,
    pub setup_seconds: u32,
    pub created_unix_ns: u64,
    pub pre_rejected_examples: u32,
    pub neutral_windows: Vec<NeutralRecordWindow>,
    pub matching_rule: Option<CalibrationMatchingRule>,
    pub surface_plan: Option<CalibrationSurfacePlanBinding>,
    pub events: Vec<CapturedCalibrationEvent>,
}

struct SurfacePromptTruth {
    prompt_id: PromptId,
    segment_index: usize,
    segment_end_ns: u64,
    evidence: Option<SurfaceEvidenceTruth>,
    position: Option<PhysicalPosition>,
    null_kind: Option<EvidenceNullKind>,
    hand: Option<Hand>,
}

struct SurfacePresentationTruth {
    prompt_id: PromptId,
    segment_index: usize,
    phase: SurfacePresentationPhase,
    planned_offset_ns: u64,
}

#[derive(Clone, Copy)]
struct SurfaceEvidenceTruth {
    role: EvidenceRole,
    block_id: u32,
}

fn surface_prompt_truth(
    binding: &CalibrationSurfacePlanBinding,
) -> Result<Vec<SurfacePromptTruth>, CalibrationCaptureError> {
    binding.validate(binding.purpose())?;
    match binding {
        CalibrationSurfacePlanBinding::Fitting { .. } => {
            let plan = SurfaceTrainingPlan::v1();
            surface_prompt_truth_from_plan(
                plan.segments(),
                plan.scoring_targets(),
                EvidencePurpose::Fitting,
            )
        }
        CalibrationSurfacePlanBinding::HeldOutValidation {
            locked_artifact_sha256,
            run_seed,
            ..
        } => {
            let plan = HeldOutSurfacePlan::v1(
                parse_sha256(locked_artifact_sha256)
                    .ok_or(CalibrationCaptureError::InvalidSurfacePlanSha256)?,
                *run_seed,
            );
            surface_prompt_truth_from_plan(
                plan.segments(),
                plan.scoring_targets(),
                EvidencePurpose::HeldOutValidation,
            )
        }
    }
}

fn surface_prompt_truth_from_plan(
    segments: &[SurfacePromptSegment],
    targets: &[SurfaceTargetMetadata],
    purpose: EvidencePurpose,
) -> Result<Vec<SurfacePromptTruth>, CalibrationCaptureError> {
    let positions = targets
        .iter()
        .map(|target| (target.prompt_id().as_str(), target.position()))
        .collect::<BTreeMap<_, _>>();
    segments
        .iter()
        .enumerate()
        .map(|(segment_index, segment)| {
            let (evidence, null_kind, position) = match (purpose, segment.kind()) {
                (EvidencePurpose::Fitting, SurfaceSegmentKind::PromptedTap) => (
                    Some(SurfaceEvidenceTruth {
                        role: EvidenceRole::Training,
                        block_id: 30,
                    }),
                    None,
                    positions.get(segment.id().as_str()).copied(),
                ),
                (EvidencePurpose::Fitting, SurfaceSegmentKind::ThresholdTap) => (
                    Some(SurfaceEvidenceTruth {
                        role: EvidenceRole::ThresholdSelection,
                        block_id: 31,
                    }),
                    None,
                    positions.get(segment.id().as_str()).copied(),
                ),
                (EvidencePurpose::Fitting, SurfaceSegmentKind::ThresholdQuiet) => (
                    Some(SurfaceEvidenceTruth {
                        role: EvidenceRole::ThresholdSelection,
                        block_id: 31,
                    }),
                    Some(EvidenceNullKind::Quiet),
                    None,
                ),
                // Inter-prompt quiet is a plan-owned capture fact used for
                // provenance and rearm. It must resolve as a known prompt but
                // must not enter either training or threshold evidence.
                (EvidencePurpose::Fitting, SurfaceSegmentKind::QuietNull) => {
                    (None, Some(EvidenceNullKind::Quiet), None)
                }
                (EvidencePurpose::HeldOutValidation, SurfaceSegmentKind::PromptedTap) => (
                    Some(SurfaceEvidenceTruth {
                        role: EvidenceRole::HeldOutValidation,
                        block_id: 40,
                    }),
                    None,
                    positions.get(segment.id().as_str()).copied(),
                ),
                (EvidencePurpose::HeldOutValidation, SurfaceSegmentKind::QuietNull) => (
                    Some(SurfaceEvidenceTruth {
                        role: EvidenceRole::HeldOutValidation,
                        block_id: 40,
                    }),
                    Some(EvidenceNullKind::Quiet),
                    None,
                ),
                (EvidencePurpose::HeldOutValidation, SurfaceSegmentKind::OrdinaryMotion) => (
                    Some(SurfaceEvidenceTruth {
                        role: EvidenceRole::HeldOutValidation,
                        block_id: 40,
                    }),
                    Some(EvidenceNullKind::OrdinaryMotion),
                    None,
                ),
                _ => {
                    return Err(CalibrationCaptureError::SurfacePromptKindMismatch(
                        segment.id().as_str().to_owned(),
                    ));
                }
            };
            let hand = position
                .map(|position| position.hand)
                .or_else(|| segment.motion_hand());
            if null_kind.is_none() && position.is_none() {
                return Err(CalibrationCaptureError::SurfacePromptKindMismatch(
                    segment.id().as_str().to_owned(),
                ));
            }
            Ok(SurfacePromptTruth {
                prompt_id: segment.id().clone(),
                segment_index,
                segment_end_ns: segment.end_ns(),
                evidence,
                position,
                null_kind,
                hand,
            })
        })
        .collect()
}

fn surface_presentation_truth(
    binding: &CalibrationSurfacePlanBinding,
) -> Result<Vec<SurfacePresentationTruth>, CalibrationCaptureError> {
    binding.validate(binding.purpose())?;
    let segments = match binding {
        CalibrationSurfacePlanBinding::Fitting { .. } => {
            SurfaceTrainingPlan::v1().segments().to_vec()
        }
        CalibrationSurfacePlanBinding::HeldOutValidation {
            locked_artifact_sha256,
            run_seed,
            ..
        } => HeldOutSurfacePlan::v1(
            parse_sha256(locked_artifact_sha256)
                .ok_or(CalibrationCaptureError::InvalidSurfacePlanSha256)?,
            *run_seed,
        )
        .segments()
        .to_vec(),
    };
    Ok(segments
        .iter()
        .enumerate()
        .flat_map(|(segment_index, segment)| {
            segment
                .scheduled_presentations()
                .map(move |presentation| SurfacePresentationTruth {
                    prompt_id: segment.id().clone(),
                    segment_index,
                    phase: presentation.phase(),
                    planned_offset_ns: presentation.at_ns(),
                })
        })
        .collect())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CaptureBuildReport {
    pub raw_keyboard_events: u32,
    pub accepted_key_downs: u32,
    pub rejected_keyboard_events: u32,
    pub matched_observations: u32,
    pub unmatched_observations: u32,
    pub matching_rule: CalibrationMatchingRule,
}

#[derive(Default)]
struct CaptureBuildCounts {
    raw_keyboard_events: u32,
    accepted_key_downs: u32,
    rejected_keyboard_events: u32,
    matched_observations: u32,
    unmatched_observations: u32,
}

/// Canonically matched capture data before final evidence-contract validation.
///
/// Live coverage checkpoints may inspect the matched observations while a
/// fitting capture is still collecting its later role families. Persisted
/// evidence must go through `build_calibration_evidence`, which consumes this
/// same match and applies the complete `CalibrationEvidence` contract.
pub(crate) struct CalibrationCaptureMatch {
    evidence_input: CalibrationEvidenceInput,
    report: CaptureBuildReport,
}

impl CalibrationCaptureMatch {
    pub(crate) fn observations(&self) -> &[CalibrationObservation] {
        &self.evidence_input.observations
    }

    fn into_evidence(
        self,
    ) -> Result<(CalibrationEvidence, CaptureBuildReport), CalibrationCaptureError> {
        let Self {
            evidence_input,
            report,
        } = self;
        let evidence =
            CalibrationEvidence::new(evidence_input).map_err(CalibrationCaptureError::Evidence)?;
        Ok((evidence, report))
    }
}

#[derive(Clone, Debug)]
struct IndexedFrame {
    record_ordinal: u64,
    host_time_ns: u64,
    frame: crate::protocol::SensorFrame,
}

/// Inclusive host-clock intervals owned by the selected physical keyboard.
///
/// Every raw event owns its timestamp. A down event additionally owns the
/// complete interval through its matching up event. An unclosed down owns the
/// rest of the capture. This makes a down/up pair causal even when replay ends
/// with a neutral key state, and makes exact sensor-window boundaries explicit.
#[derive(Default)]
struct SelectedKeyboardActivity {
    intervals: Vec<HostTimeInterval>,
}

#[derive(Clone, Copy)]
struct HostTimeInterval {
    start_ns: u64,
    end_ns: u64,
}

impl SelectedKeyboardActivity {
    fn from_journal(journal: &CalibrationCaptureJournal) -> Result<Self, CalibrationCaptureError> {
        let mut active_keys = BTreeMap::<u16, u64>::new();
        let mut intervals = Vec::new();
        let mut previous_host_time_ns = None;
        for captured in &journal.events {
            let event = match captured {
                CapturedCalibrationEvent::Keyboard { event, .. }
                | CapturedCalibrationEvent::KeyboardStateOnly { event, .. } => event,
                CapturedCalibrationEvent::DisplayAcknowledged { .. }
                | CapturedCalibrationEvent::Idle { .. } => continue,
            };
            if previous_host_time_ns.is_some_and(|previous| event.host_time_ns < previous) {
                return Err(CalibrationCaptureError::KeyboardEventTimeRegression {
                    previous_host_time_ns: previous_host_time_ns
                        .expect("a regressing event has a predecessor"),
                    current_host_time_ns: event.host_time_ns,
                });
            }
            previous_host_time_ns = Some(event.host_time_ns);
            match event.state {
                RawKeyState::Down => {
                    active_keys
                        .entry(event.physical_code)
                        .or_insert(event.host_time_ns);
                }
                RawKeyState::Up => {
                    let start_ns = active_keys
                        .remove(&event.physical_code)
                        .unwrap_or(event.host_time_ns);
                    intervals.push(HostTimeInterval {
                        start_ns,
                        end_ns: event.host_time_ns,
                    });
                }
                RawKeyState::Repeat => {
                    // A repeat is activity even if capture began after its down.
                    intervals.push(HostTimeInterval {
                        start_ns: event.host_time_ns,
                        end_ns: event.host_time_ns,
                    });
                }
            }
        }
        intervals.extend(active_keys.into_values().map(|start_ns| HostTimeInterval {
            start_ns,
            end_ns: u64::MAX,
        }));
        intervals.sort_by_key(|interval| interval.start_ns);

        let mut merged = Vec::<HostTimeInterval>::new();
        for interval in intervals {
            if let Some(previous) = merged.last_mut()
                && interval.start_ns <= previous.end_ns.saturating_add(1)
            {
                previous.end_ns = previous.end_ns.max(interval.end_ns);
            } else {
                merged.push(interval);
            }
        }
        Ok(Self { intervals: merged })
    }

    fn overlaps(&self, start_ns: u64, end_ns: u64) -> bool {
        let preceding = self
            .intervals
            .partition_point(|interval| interval.start_ns <= end_ns);
        preceding != 0 && self.intervals[preceding - 1].end_ns >= start_ns
    }

    fn overlaps_frame_window(&self, hand_frames: &[&IndexedFrame], end_index: usize) -> bool {
        let frames = window_frames_at(hand_frames, end_index);
        self.overlaps(
            frames
                .first()
                .expect("a causal window has a first frame")
                .host_time_ns,
            frames
                .last()
                .expect("a causal window has a last frame")
                .host_time_ns,
        )
    }

    fn overlaps_record_window(
        &self,
        frames: &[IndexedFrame],
        start_record: u64,
        end_record_exclusive: u64,
    ) -> Option<(u64, u64)> {
        let start = frames.partition_point(|frame| frame.record_ordinal < start_record);
        let end = frames.partition_point(|frame| frame.record_ordinal < end_record_exclusive);
        let selected = frames.get(start..end)?;
        let first = selected.first()?;
        let (start_ns, end_ns) = selected[1..].iter().fold(
            (first.host_time_ns, first.host_time_ns),
            |(start_ns, end_ns), frame| {
                (
                    start_ns.min(frame.host_time_ns),
                    end_ns.max(frame.host_time_ns),
                )
            },
        );
        self.overlaps(start_ns, end_ns)
            .then_some((start_ns, end_ns))
    }
}

/// Source frames already owned by a selected causal window.
///
/// Record ordinals are unique across both hands, so reserving every record in
/// a window rejects same-hand overlap without blocking simultaneous windows
/// from the other controller.
#[derive(Default)]
struct WindowReservations {
    frame_records: BTreeSet<u64>,
}

impl WindowReservations {
    fn is_available(&self, hand_frames: &[&IndexedFrame], end_index: usize) -> bool {
        window_frames_at(hand_frames, end_index)
            .iter()
            .all(|frame| !self.frame_records.contains(&frame.record_ordinal))
    }

    fn reserve(&mut self, hand_frames: &[&IndexedFrame], end_index: usize) {
        self.frame_records.extend(
            window_frames_at(hand_frames, end_index)
                .iter()
                .map(|frame| frame.record_ordinal),
        );
    }

    fn end_index_for_record(
        hand_frames: &[&IndexedFrame],
        window_end_record: u64,
    ) -> Option<usize> {
        hand_frames
            .binary_search_by_key(&window_end_record, |frame| frame.record_ordinal)
            .ok()
            .filter(|end_index| *end_index >= crate::pipeline::WINDOW_FRAMES.saturating_sub(1))
    }

    #[cfg(test)]
    fn record_window_is_available(
        &self,
        hand_frames: &[&IndexedFrame],
        window_end_record: u64,
    ) -> bool {
        Self::end_index_for_record(hand_frames, window_end_record)
            .is_some_and(|end_index| self.is_available(hand_frames, end_index))
    }

    fn reserve_record_window(
        &mut self,
        hand_frames: &[&IndexedFrame],
        window_end_record: u64,
    ) -> bool {
        let Some(end_index) = Self::end_index_for_record(hand_frames, window_end_record) else {
            return false;
        };
        if !self.is_available(hand_frames, end_index) {
            return false;
        }
        self.reserve(hand_frames, end_index);
        true
    }
}

#[derive(Clone, Copy, Debug)]
struct CaptureCandidate {
    role: EvidenceRole,
    domain: EvidenceDomain,
    block_id: u32,
    position: Option<PhysicalPosition>,
    null_kind: Option<EvidenceNullKind>,
    hand: Hand,
    trigger_host_time_ns: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CausalContactEvent {
    hand: Hand,
    finger: Finger,
    host_time_ns: u64,
    window_end_record: u64,
}

/// Match raw labels and plan-bound cue acknowledgements to causal sensor windows.
pub fn build_calibration_evidence(
    manifest: &AcquisitionManifest,
    clocks: &ClockEpochSet,
    journal: &CalibrationCaptureJournal,
    session: &FinalizedSession,
) -> Result<(CalibrationEvidence, CaptureBuildReport), CalibrationCaptureError> {
    let qualified = qualify_capture_clocks(manifest, clocks, session)?;
    build_calibration_evidence_qualified(&qualified, journal)
}

pub(crate) fn build_calibration_evidence_qualified(
    qualified: &QualifiedOfflineClocks<'_>,
    journal: &CalibrationCaptureJournal,
) -> Result<(CalibrationEvidence, CaptureBuildReport), CalibrationCaptureError> {
    match_calibration_capture_qualified(qualified, journal)?.into_evidence()
}

pub(crate) fn qualify_capture_clocks<'a>(
    manifest: &'a AcquisitionManifest,
    clocks: &'a ClockEpochSet,
    session: &'a FinalizedSession,
) -> Result<QualifiedOfflineClocks<'a>, CalibrationCaptureError> {
    reject_control_records(session.summary())?;
    clocks
        .qualify_session(manifest, session)
        .map_err(|error| match error {
            ClockEpochFitError::Epoch(error) => CalibrationCaptureError::Clock(error),
            error => CalibrationCaptureError::ClockQualification(error),
        })
}

/// Run the canonical matcher without requiring a capture's later role families.
pub(crate) fn match_calibration_capture_qualified(
    qualified: &QualifiedOfflineClocks<'_>,
    journal: &CalibrationCaptureJournal,
) -> Result<CalibrationCaptureMatch, CalibrationCaptureError> {
    let manifest = qualified.manifest();
    let clocks = qualified.epochs();
    let session = qualified.session();
    journal.validate()?;
    manifest
        .validate()
        .map_err(CalibrationCaptureError::Manifest)?;
    let manifest_sha256 = manifest
        .sha256()
        .map_err(CalibrationCaptureError::Manifest)?;
    let session_sha256 = session.sha256();
    let capture_journal_sha256 = sha256(&journal.canonical_json()?);
    let clock_epochs_sha256 = clocks
        .fingerprint()
        .map_err(CalibrationCaptureError::Clock)?;
    if parse_sha256(&journal.acquisition_manifest_sha256) != Some(manifest_sha256) {
        return Err(CalibrationCaptureError::ManifestIdentityMismatch);
    }
    if parse_sha256(&journal.session_sha256) != Some(session_sha256) {
        return Err(CalibrationCaptureError::SessionIdentityMismatch);
    }
    if parse_sha256(&journal.clock_epochs_sha256) != Some(clock_epochs_sha256) {
        return Err(CalibrationCaptureError::ClockIdentityMismatch);
    }

    if session.header().session_id != journal.session_id {
        return Err(CalibrationCaptureError::SessionIdentityMismatch);
    }
    reject_control_records(session.summary())?;
    manifest
        .validate_observed_topology(
            session.summary().left.received,
            session.summary().right.received,
        )
        .map_err(CalibrationCaptureError::Topology)?;
    if !session.summary().is_clean_two_hand_sensor_stream() {
        return Err(CalibrationCaptureError::StreamIntegrity);
    }
    let frames = index_frames(manifest, session)?;
    let left_frames = frames
        .iter()
        .filter(|candidate| candidate.frame.hand == Hand::Left)
        .collect::<Vec<_>>();
    let right_frames = frames
        .iter()
        .filter(|candidate| candidate.frame.hand == Hand::Right)
        .collect::<Vec<_>>();
    let keyboard_activity = SelectedKeyboardActivity::from_journal(journal)?;
    let mut counts = CaptureBuildCounts::default();
    let (candidates, key_state_neutral) = capture_candidates(journal, &mut counts)?;
    let matching_rule = match journal.purpose {
        EvidencePurpose::Fitting => {
            matching_rule_for_fitting(journal, &frames, &keyboard_activity)?
        }
        EvidencePurpose::HeldOutValidation => {
            let rule = journal
                .matching_rule
                .clone()
                .ok_or(CalibrationCaptureError::MissingMatchingRule)?;
            rule.validate()?;
            if parse_sha256(&rule.acquisition_manifest_sha256) != Some(manifest_sha256) {
                return Err(CalibrationCaptureError::MatchingRuleManifestIdentityMismatch);
            }
            rule
        }
    };
    let calibration_matching_rule_sha256 = matching_rule.fingerprint()?;
    let contacts = detect_causal_contacts(&frames, &matching_rule);

    let mut window_reservations = WindowReservations::default();
    let mut observations = Vec::new();
    for candidate in candidates {
        let hand_frames = match candidate.hand {
            Hand::Left => &left_frames,
            Hand::Right => &right_frames,
        };
        let selected = if candidate.null_kind == Some(EvidenceNullKind::OrdinaryMotion) {
            select_measured_motion_window_end(
                hand_frames,
                candidate.trigger_host_time_ns,
                thresholds_for_hand(&matching_rule, candidate.hand),
                &mut window_reservations,
                &keyboard_activity,
            )
        } else if candidate.domain == EvidenceDomain::Keyboard {
            let position = candidate
                .position
                .expect("keyboard capture candidates always have a position");
            select_training_peak(
                hand_frames,
                position,
                candidate.trigger_host_time_ns,
                &mut window_reservations,
            )
            .map(|(record, _)| record)
        } else {
            select_causal_window_end(
                hand_frames,
                candidate.trigger_host_time_ns,
                &mut window_reservations,
                &keyboard_activity,
            )
        };
        let Some(window_end_record) = selected else {
            counts.unmatched_observations = counts.unmatched_observations.saturating_add(1);
            continue;
        };
        observations.push(CalibrationObservation {
            role: candidate.role,
            domain: candidate.domain,
            block_id: candidate.block_id,
            position: candidate.position,
            null_kind: candidate.null_kind,
            hand: candidate.hand,
            window_end_record,
            trigger_host_time_ns: candidate.trigger_host_time_ns,
        });
        counts.matched_observations = counts.matched_observations.saturating_add(1);
    }

    let surface_observations = match_prompted_contacts(
        journal,
        &contacts,
        &matching_rule,
        &left_frames,
        &right_frames,
        &mut window_reservations,
        &keyboard_activity,
    )?;
    counts.matched_observations = counts
        .matched_observations
        .saturating_add(u32::try_from(surface_observations.len()).unwrap_or(u32::MAX));
    observations.extend(surface_observations);

    let report = CaptureBuildReport {
        raw_keyboard_events: counts.raw_keyboard_events,
        accepted_key_downs: counts.accepted_key_downs,
        rejected_keyboard_events: counts.rejected_keyboard_events,
        matched_observations: counts.matched_observations,
        unmatched_observations: counts.unmatched_observations,
        matching_rule,
    };
    Ok(CalibrationCaptureMatch {
        evidence_input: CalibrationEvidenceInput {
            purpose: journal.purpose,
            acquisition_manifest_sha256: manifest_sha256,
            clock_epochs_sha256,
            session_sha256,
            capture_journal_sha256,
            calibration_matching_rule_sha256,
            session_id: journal.session_id,
            keyboard: journal.keyboard.clone(),
            setup_seconds: journal.setup_seconds,
            created_unix_ns: journal.created_unix_ns,
            rejected_examples: journal
                .pre_rejected_examples
                .saturating_add(counts.rejected_keyboard_events)
                .saturating_add(counts.unmatched_observations),
            ordinary_motion_exposure_ms: journal.surface_plan.as_ref().map_or(
                0,
                CalibrationSurfacePlanBinding::planned_ordinary_motion_exposure_ms,
            ),
            key_state_neutral,
            observations,
        },
        report,
    })
}

fn capture_candidates(
    journal: &CalibrationCaptureJournal,
    counts: &mut CaptureBuildCounts,
) -> Result<(Vec<CaptureCandidate>, bool), CalibrationCaptureError> {
    let mut filter = KeyboardLabelFilter::new(&journal.keyboard_device_id);
    let mut candidates = Vec::new();
    let mut pending_press: Option<(u16, usize)> = None;
    let surface_truth = journal
        .surface_plan
        .as_ref()
        .map(surface_prompt_truth)
        .transpose()?
        .unwrap_or_default();
    let truth_by_id = surface_truth
        .iter()
        .map(|truth| (truth.prompt_id.as_str(), truth))
        .collect::<BTreeMap<_, _>>();
    for captured in &journal.events {
        let candidate = match captured {
            CapturedCalibrationEvent::Keyboard {
                role,
                domain,
                block_id,
                event,
            }
            | CapturedCalibrationEvent::KeyboardStateOnly {
                role,
                domain,
                block_id,
                event,
            } => {
                counts.raw_keyboard_events = counts.raw_keyboard_events.saturating_add(1);
                let disposition = filter.process(event.clone());
                let completed_candidate_index = if event.device_id == journal.keyboard_device_id
                    && event.state == RawKeyState::Up
                    && pending_press
                        .as_ref()
                        .is_some_and(|(physical_code, _)| *physical_code == event.physical_code)
                {
                    pending_press.take().map(|(_, index)| index)
                } else {
                    None
                };
                match disposition {
                    LabelDisposition::Accepted(label) if label.state == RawKeyState::Down => {
                        if matches!(captured, CapturedCalibrationEvent::Keyboard { .. })
                            && keyboard_warm_start_eligible(label.position)
                        {
                            // Keep the down's original place and provenance, but
                            // only a clean release can retain this candidate.
                            pending_press = Some((label.physical_code, candidates.len()));
                            Some(CaptureCandidate {
                                role: *role,
                                domain: *domain,
                                block_id: *block_id,
                                position: Some(label.position),
                                null_kind: None,
                                hand: label.position.hand,
                                trigger_host_time_ns: label.host_time_ns,
                            })
                        } else {
                            counts.rejected_keyboard_events =
                                counts.rejected_keyboard_events.saturating_add(1);
                            None
                        }
                    }
                    LabelDisposition::Accepted(_) => {
                        if completed_candidate_index.is_some() {
                            counts.accepted_key_downs = counts.accepted_key_downs.saturating_add(1);
                        }
                        None
                    }
                    LabelDisposition::Rejected(_) | LabelDisposition::Modifier => {
                        if matches!(disposition, LabelDisposition::Rejected(_)) {
                            counts.rejected_keyboard_events =
                                counts.rejected_keyboard_events.saturating_add(1);
                        }
                        if let Some(index) = completed_candidate_index {
                            candidates.remove(index);
                            counts.rejected_keyboard_events =
                                counts.rejected_keyboard_events.saturating_add(1);
                        }
                        None
                    }
                }
            }
            CapturedCalibrationEvent::DisplayAcknowledged { .. } => None,
            CapturedCalibrationEvent::Idle {
                prompt_id,
                sample_ordinal,
                trigger_host_time_ns,
                ..
            } => {
                let truth = truth_by_id
                    .get(prompt_id.as_str())
                    .copied()
                    .ok_or_else(|| {
                        CalibrationCaptureError::UnknownSurfacePrompt(prompt_id.as_str().to_owned())
                    })?;
                truth.evidence.map(|evidence| CaptureCandidate {
                    role: evidence.role,
                    domain: EvidenceDomain::PassiveSurface,
                    block_id: evidence.block_id,
                    position: None,
                    null_kind: truth.null_kind,
                    hand: truth.hand.unwrap_or_else(|| {
                        if sample_ordinal % 2 == 0 {
                            Hand::Left
                        } else {
                            Hand::Right
                        }
                    }),
                    trigger_host_time_ns: *trigger_host_time_ns,
                })
            }
        };
        if let Some(candidate) = candidate {
            candidates.push(candidate);
        }
    }
    if let Some((_, index)) = pending_press {
        candidates.remove(index);
        counts.rejected_keyboard_events = counts.rejected_keyboard_events.saturating_add(1);
    }
    Ok((candidates, filter.held_count() == 0))
}

fn matching_rule_for_fitting(
    journal: &CalibrationCaptureJournal,
    frames: &[IndexedFrame],
    keyboard_activity: &SelectedKeyboardActivity,
) -> Result<CalibrationMatchingRule, CalibrationCaptureError> {
    for neutral in &journal.neutral_windows {
        if let Some((start_host_time_ns, end_host_time_ns)) = keyboard_activity
            .overlaps_record_window(frames, neutral.start_record, neutral.end_record_exclusive)
        {
            return Err(
                CalibrationCaptureError::KeyboardActivityOverlapsPassiveEvidence {
                    start_host_time_ns,
                    end_host_time_ns,
                },
            );
        }
    }
    let NeutralContactRuleSources {
        neutral_windows_sha256,
        neutral_source_sha256,
        thresholds,
    } = derive_neutral_thresholds(&journal.neutral_windows, frames)?;
    if let Some(rule) = &journal.matching_rule {
        if rule.acquisition_manifest_sha256 != journal.acquisition_manifest_sha256 {
            return Err(CalibrationCaptureError::MatchingRuleManifestIdentityMismatch);
        }
        if rule.neutral_windows_sha256 != format_sha256(&neutral_windows_sha256)
            || rule.neutral_source_sha256 != format_sha256(&neutral_source_sha256)
            || rule.thresholds != thresholds
        {
            return Err(CalibrationCaptureError::MatchingRuleNeutralEvidenceMismatch);
        }
        return Ok(rule.clone());
    }

    matching_rule_from_neutral_sources(
        journal.acquisition_manifest_sha256.clone(),
        neutral_windows_sha256,
        neutral_source_sha256,
        thresholds,
    )
}

fn matching_rule_from_neutral_sources(
    acquisition_manifest_sha256: String,
    neutral_windows_sha256: [u8; 32],
    neutral_source_sha256: [u8; 32],
    thresholds: [HandContactThresholds; 2],
) -> Result<CalibrationMatchingRule, CalibrationCaptureError> {
    let rule = CalibrationMatchingRule {
        schema: CALIBRATION_MATCHING_RULE_SCHEMA.to_owned(),
        acquisition_manifest_sha256,
        neutral_windows_sha256: format_sha256(&neutral_windows_sha256),
        neutral_source_sha256: format_sha256(&neutral_source_sha256),
        feature_schema: CONTACT_FEATURE_SCHEMA.to_owned(),
        thresholds,
        debounce_windows: CONTACT_DEBOUNCE_WINDOWS,
        rearm_windows: CONTACT_REARM_WINDOWS,
        response_start_ns: CONTACT_RESPONSE_START_NS,
        response_end_ns: CONTACT_RESPONSE_END_NS,
    };
    rule.validate()?;
    Ok(rule)
}

struct NeutralContactRuleSources {
    neutral_windows_sha256: [u8; 32],
    neutral_source_sha256: [u8; 32],
    thresholds: [HandContactThresholds; 2],
}

fn derive_neutral_thresholds(
    neutral_windows: &[NeutralRecordWindow],
    frames: &[IndexedFrame],
) -> Result<NeutralContactRuleSources, CalibrationCaptureError> {
    let neutral_bytes = serde_json::to_vec(neutral_windows)
        .map_err(|error| CalibrationCaptureError::Serialization(error.to_string()))?;
    let neutral_windows_sha256 = sha256(&neutral_bytes);
    let mut neutral_source_bytes = Vec::new();
    for frame in frames.iter().filter(|frame| {
        neutral_windows.iter().any(|window| {
            frame.record_ordinal >= window.start_record
                && frame.record_ordinal < window.end_record_exclusive
        })
    }) {
        neutral_source_bytes.extend_from_slice(&frame.record_ordinal.to_le_bytes());
        neutral_source_bytes.extend_from_slice(&frame.host_time_ns.to_le_bytes());
        neutral_source_bytes.extend_from_slice(
            &crate::protocol::encode_frame(&frame.frame)
                .map_err(|error| CalibrationCaptureError::Serialization(error.to_string()))?,
        );
    }
    let neutral_source_sha256 = sha256(&neutral_source_bytes);
    let derive_hand = |hand| {
        let mut maximum_contact_energy = 0_u64;
        let mut maximum_motion_energy = 0_u64;
        let mut window_count = 0_u64;
        for neutral in neutral_windows {
            let selected = frames
                .iter()
                .filter(|frame| {
                    frame.record_ordinal >= neutral.start_record
                        && frame.record_ordinal < neutral.end_record_exclusive
                        && frame.frame.hand == hand
                })
                .collect::<Vec<_>>();
            for end_index in crate::pipeline::WINDOW_FRAMES.saturating_sub(1)..selected.len() {
                // Whole-stream raw admission in index_frames proves strict
                // aggregate and sensor progress. Selection preserves record
                // order, and this loop takes a nonempty window from one hand.
                let window = TapWindow::new(
                    window_frames_at(&selected, end_index)
                        .iter()
                        .map(|frame| frame.frame)
                        .collect(),
                )
                .expect("raw admission proves nonempty one-hand strictly advancing windows");
                if !HandDecoder::usable_for_contact(&window) {
                    continue;
                }
                maximum_contact_energy = maximum_contact_energy.max(
                    HandDecoder::causal_contact_energies(&window)
                        .into_iter()
                        .max()
                        .unwrap_or(0),
                );
                maximum_motion_energy =
                    maximum_motion_energy.max(HandDecoder::common_motion_energy(&window));
                window_count = window_count.saturating_add(1);
            }
        }
        if window_count == 0 {
            return Err(CalibrationCaptureError::MissingNeutralContactEvidence(hand));
        }
        let low_energy = maximum_contact_energy
            .saturating_mul(4)
            .max(CONTACT_LOW_ENERGY_FLOOR);
        let high_energy = maximum_contact_energy
            .saturating_mul(16)
            .max(CONTACT_HIGH_ENERGY_FLOOR)
            .max(low_energy.saturating_add(1));
        Ok(HandContactThresholds {
            hand,
            low_energy,
            high_energy,
            motion_energy: maximum_motion_energy
                .saturating_mul(16)
                .max(MOTION_ENERGY_FLOOR),
        })
    };
    Ok(NeutralContactRuleSources {
        neutral_windows_sha256,
        neutral_source_sha256,
        thresholds: [derive_hand(Hand::Left)?, derive_hand(Hand::Right)?],
    })
}

/// Derive the one frozen contact rule from canonical neutral source records.
pub fn derive_matching_rule(
    manifest: &AcquisitionManifest,
    session: &FinalizedSession,
    neutral_windows: &[NeutralRecordWindow],
) -> Result<CalibrationMatchingRule, CalibrationCaptureError> {
    manifest
        .validate()
        .map_err(CalibrationCaptureError::Manifest)?;
    let frames = index_frames(manifest, session)?;
    let NeutralContactRuleSources {
        neutral_windows_sha256,
        neutral_source_sha256,
        thresholds,
    } = derive_neutral_thresholds(neutral_windows, &frames)?;
    matching_rule_from_neutral_sources(
        format_sha256(
            &manifest
                .sha256()
                .map_err(CalibrationCaptureError::Manifest)?,
        ),
        neutral_windows_sha256,
        neutral_source_sha256,
        thresholds,
    )
}

/// Recompute a persisted contact rule from its immutable neutral source.
///
/// This is the fitting admission boundary. A valid JSON rule alone is not
/// enough to influence the final decoder artifact.
pub fn verify_matching_rule_sources(
    rule: &CalibrationMatchingRule,
    manifest: &AcquisitionManifest,
    session: &FinalizedSession,
    neutral_windows: &[NeutralRecordWindow],
) -> Result<(), CalibrationCaptureError> {
    rule.validate()?;
    let manifest_sha256 = manifest
        .sha256()
        .map_err(CalibrationCaptureError::Manifest)?;
    if parse_sha256(&rule.acquisition_manifest_sha256) != Some(manifest_sha256) {
        return Err(CalibrationCaptureError::MatchingRuleManifestIdentityMismatch);
    }
    let frames = index_frames(manifest, session)?;
    let NeutralContactRuleSources {
        neutral_windows_sha256,
        neutral_source_sha256,
        thresholds,
    } = derive_neutral_thresholds(neutral_windows, &frames)?;
    if parse_sha256(&rule.neutral_windows_sha256) != Some(neutral_windows_sha256)
        || parse_sha256(&rule.neutral_source_sha256) != Some(neutral_source_sha256)
        || rule.thresholds != thresholds
    {
        return Err(CalibrationCaptureError::MatchingRuleNeutralEvidenceMismatch);
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ContactDetectorState {
    Armed { above_high: u8 },
    AwaitingRearm { below_low: u8 },
}

fn detect_causal_contacts(
    frames: &[IndexedFrame],
    rule: &CalibrationMatchingRule,
) -> Vec<CausalContactEvent> {
    let mut buffers: [Vec<&IndexedFrame>; 2] = std::array::from_fn(|_| Vec::new());
    let mut states = [ContactDetectorState::Armed { above_high: 0 }; 2];
    let mut events = Vec::new();
    for frame in frames {
        let hand_index = match frame.frame.hand {
            Hand::Left => 0,
            Hand::Right => 1,
        };
        let buffer = &mut buffers[hand_index];
        buffer.push(frame);
        if buffer.len() > crate::pipeline::WINDOW_FRAMES {
            buffer.remove(0);
        }
        if buffer.len() != crate::pipeline::WINDOW_FRAMES {
            continue;
        }
        let Ok(window) = TapWindow::new(buffer.iter().map(|frame| frame.frame).collect()) else {
            states[hand_index] = ContactDetectorState::Armed { above_high: 0 };
            continue;
        };
        if !HandDecoder::usable_for_contact(&window) {
            states[hand_index] = ContactDetectorState::Armed { above_high: 0 };
            continue;
        }
        let energies = HandDecoder::causal_contact_energies(&window);
        let (sensor, strongest) = energies
            .into_iter()
            .enumerate()
            .max_by_key(|(sensor, energy)| (*energy, std::cmp::Reverse(*sensor)))
            .expect("five finger energies exist");
        let thresholds = rule.thresholds[hand_index];
        states[hand_index] = match states[hand_index] {
            ContactDetectorState::Armed { above_high } if strongest >= thresholds.high_energy => {
                let above_high = above_high.saturating_add(1);
                if above_high >= rule.debounce_windows {
                    events.push(CausalContactEvent {
                        hand: frame.frame.hand,
                        finger: Finger::from_sensor_index(sensor)
                            .expect("contact detector uses only finger sensors"),
                        host_time_ns: frame.host_time_ns,
                        window_end_record: frame.record_ordinal,
                    });
                    ContactDetectorState::AwaitingRearm { below_low: 0 }
                } else {
                    ContactDetectorState::Armed { above_high }
                }
            }
            ContactDetectorState::Armed { .. } => ContactDetectorState::Armed { above_high: 0 },
            ContactDetectorState::AwaitingRearm { below_low }
                if strongest <= thresholds.low_energy =>
            {
                let below_low = below_low.saturating_add(1);
                if below_low >= rule.rearm_windows {
                    ContactDetectorState::Armed { above_high: 0 }
                } else {
                    ContactDetectorState::AwaitingRearm { below_low }
                }
            }
            ContactDetectorState::AwaitingRearm { .. } => {
                ContactDetectorState::AwaitingRearm { below_low: 0 }
            }
        };
    }
    events.sort_by_key(|event| (event.host_time_ns, event.window_end_record));
    events
}

fn match_prompted_contacts(
    journal: &CalibrationCaptureJournal,
    contacts: &[CausalContactEvent],
    rule: &CalibrationMatchingRule,
    left_frames: &[&IndexedFrame],
    right_frames: &[&IndexedFrame],
    reservations: &mut WindowReservations,
    keyboard_activity: &SelectedKeyboardActivity,
) -> Result<Vec<CalibrationObservation>, CalibrationCaptureError> {
    let mut observations = Vec::new();
    let truths = journal
        .surface_plan
        .as_ref()
        .map(surface_prompt_truth)
        .transpose()?
        .unwrap_or_default();
    let truth_by_id = truths
        .iter()
        .map(|truth| (truth.prompt_id.as_str(), truth))
        .collect::<BTreeMap<_, _>>();
    let presentation_truths = journal
        .surface_plan
        .as_ref()
        .map(surface_presentation_truth)
        .transpose()?
        .unwrap_or_default();
    let mut next_presentation = 0_usize;
    for event in &journal.events {
        let CapturedCalibrationEvent::DisplayAcknowledged {
            prompt_id,
            presentation,
        } = event
        else {
            continue;
        };
        let presentation_truth = presentation_truths.get(next_presentation).ok_or_else(|| {
            CalibrationCaptureError::DuplicateSurfacePrompt(prompt_id.as_str().to_owned())
        })?;
        next_presentation += 1;
        if presentation_truth.prompt_id != *prompt_id {
            return Err(CalibrationCaptureError::ReorderedSurfacePrompt {
                expected: presentation_truth.prompt_id.as_str().to_owned(),
                actual: prompt_id.as_str().to_owned(),
            });
        }
        if presentation_truth.phase != SurfacePresentationPhase::Cue {
            continue;
        }
        let truth = truth_by_id
            .get(prompt_id.as_str())
            .copied()
            .ok_or_else(|| {
                CalibrationCaptureError::UnknownSurfacePrompt(prompt_id.as_str().to_owned())
            })?;
        let position = truth.position.ok_or_else(|| {
            CalibrationCaptureError::SurfacePromptKindMismatch(prompt_id.as_str().to_owned())
        })?;
        let evidence = truth.evidence.ok_or_else(|| {
            CalibrationCaptureError::SurfacePromptKindMismatch(prompt_id.as_str().to_owned())
        })?;
        let first = associate_prompted_contact(
            position,
            *presentation,
            contacts,
            |contact| {
                let hand_frames = match contact.hand {
                    Hand::Left => left_frames,
                    Hand::Right => right_frames,
                };
                let Some(end_index) = WindowReservations::end_index_for_record(
                    hand_frames,
                    contact.window_end_record,
                ) else {
                    return false;
                };
                reservations.is_available(hand_frames, end_index)
                    && !keyboard_activity.overlaps_frame_window(hand_frames, end_index)
            },
            rule,
        )?;
        let hand_frames = match first.hand {
            Hand::Left => left_frames,
            Hand::Right => right_frames,
        };
        assert!(
            reservations.reserve_record_window(hand_frames, first.window_end_record),
            "an available detector contact owns one complete source window"
        );
        observations.push(CalibrationObservation {
            role: evidence.role,
            domain: EvidenceDomain::PassiveSurface,
            block_id: evidence.block_id,
            position: Some(position),
            null_kind: None,
            hand: first.hand,
            window_end_record: first.window_end_record,
            // Contact time is sensor-derived. The presentation receipt only
            // bounded the association search above.
            trigger_host_time_ns: first.host_time_ns,
        });
    }
    Ok(observations)
}

fn associate_prompted_contact(
    position: PhysicalPosition,
    presentation: SurfacePresentationReceipt,
    contacts: &[CausalContactEvent],
    is_available: impl Fn(&CausalContactEvent) -> bool,
    rule: &CalibrationMatchingRule,
) -> Result<CausalContactEvent, CalibrationCaptureError> {
    let response_start = presentation
        .acknowledged_host_time_ns()
        .checked_add(rule.response_start_ns)
        .ok_or(CalibrationCaptureError::SurfaceResponseOverflow)?;
    let response_end = presentation
        .acknowledged_host_time_ns()
        .checked_add(rule.response_end_ns)
        .ok_or(CalibrationCaptureError::SurfaceResponseOverflow)?;
    let mut response = contacts
        .iter()
        .filter(|contact| {
            is_available(contact)
                && contact.host_time_ns >= response_start
                && contact.host_time_ns <= response_end
        })
        .copied();
    let first = response
        .next()
        .ok_or(CalibrationCaptureError::NoSurfaceContact {
            position,
            response_start_ns: response_start,
            response_end_ns: response_end,
        })?;
    if first.hand != position.hand {
        return Err(CalibrationCaptureError::WrongSurfaceHand {
            position,
            observed: first.hand,
        });
    }
    if first.finger != position.finger {
        return Err(CalibrationCaptureError::WrongSurfaceFinger {
            position,
            observed: first.finger,
        });
    }
    let additional_contacts = response.count();
    if additional_contacts != 0 {
        return Err(CalibrationCaptureError::MultipleSurfaceContacts {
            position,
            actual: additional_contacts + 1,
        });
    }
    Ok(first)
}

fn thresholds_for_hand(rule: &CalibrationMatchingRule, hand: Hand) -> HandContactThresholds {
    rule.thresholds[match hand {
        Hand::Left => 0,
        Hand::Right => 1,
    }]
}

fn index_frames(
    manifest: &AcquisitionManifest,
    session: &FinalizedSession,
) -> Result<Vec<IndexedFrame>, CalibrationCaptureError> {
    let canonicalizer =
        FrameCanonicalizer::new(manifest).map_err(CalibrationCaptureError::Manifest)?;
    let mut reader = session.reader();
    let mut frames = Vec::new();
    let mut raw_integrity = [FrameIntegrity::default(), FrameIntegrity::default()];
    let mut record_ordinal = 0_u64;
    while let Some(record) = reader
        .next_record()
        .map_err(CalibrationCaptureError::Session)?
    {
        match record.kind {
            SessionRecordKind::Decoded => {
                let frame =
                    record
                        .decode()
                        .map_err(|error| CalibrationCaptureError::DecodedRecord {
                            record_ordinal,
                            message: error.to_string(),
                        })?;
                let frame = canonicalizer.canonicalize(&frame).map_err(|error| {
                    CalibrationCaptureError::FrameCanonicalization {
                        record_ordinal,
                        error,
                    }
                })?;
                raw_integrity[frame.hand.index()]
                    .observe(&frame)
                    .map_err(|error| match error {
                        FrameIntegrityError::Quality(reason) => {
                            CalibrationCaptureError::RawFrameQuality {
                                record_ordinal,
                                hand: frame.hand,
                                reason,
                            }
                        }
                        FrameIntegrityError::Structure(error) => {
                            CalibrationCaptureError::RawFrameStructure {
                                record_ordinal,
                                hand: frame.hand,
                                error,
                            }
                        }
                    })?;
                frames.push(IndexedFrame {
                    record_ordinal,
                    host_time_ns: record.host_time_ns,
                    frame,
                });
            }
            SessionRecordKind::Malformed
            | SessionRecordKind::Oversized
            | SessionRecordKind::Unterminated => {
                // The clean-stream summary check rejects these before frame
                // indexing. Keep the taxonomy exhaustive instead of skipping
                // future record kinds implicitly.
            }
            SessionRecordKind::QuiesceRequest | SessionRecordKind::QuiesceReceipt => {
                return Err(CalibrationCaptureError::UnexpectedControlRecord { kind: record.kind });
            }
        }
        record_ordinal += 1;
    }
    Ok(frames)
}

fn reject_control_records(
    summary: &crate::session::SessionSummary,
) -> Result<(), CalibrationCaptureError> {
    if summary.quiesce_requests != 0 {
        return Err(CalibrationCaptureError::UnexpectedControlRecord {
            kind: SessionRecordKind::QuiesceRequest,
        });
    }
    if summary.quiesce_receipts != 0 {
        return Err(CalibrationCaptureError::UnexpectedControlRecord {
            kind: SessionRecordKind::QuiesceReceipt,
        });
    }
    Ok(())
}

fn select_causal_window_end(
    hand_frames: &[&IndexedFrame],
    trigger_host_time_ns: u64,
    reservations: &mut WindowReservations,
    keyboard_activity: &SelectedKeyboardActivity,
) -> Option<u64> {
    let response_end = trigger_host_time_ns.checked_add(CONTACT_RESPONSE_END_NS)?;
    for end_index in candidate_end_indices(
        hand_frames,
        i128::from(trigger_host_time_ns),
        i128::from(response_end),
    ) {
        let end = hand_frames[end_index];
        if !reservations.is_available(hand_frames, end_index)
            || keyboard_activity.overlaps_frame_window(hand_frames, end_index)
        {
            continue;
        }
        if tap_window_at(hand_frames, end_index).is_none() {
            continue;
        }
        reservations.reserve(hand_frames, end_index);
        return Some(end.record_ordinal);
    }
    None
}

fn select_measured_motion_window_end(
    hand_frames: &[&IndexedFrame],
    trigger_host_time_ns: u64,
    thresholds: HandContactThresholds,
    reservations: &mut WindowReservations,
    keyboard_activity: &SelectedKeyboardActivity,
) -> Option<u64> {
    let response_end = trigger_host_time_ns.checked_add(CONTACT_RESPONSE_END_NS)?;
    for end_index in candidate_end_indices(
        hand_frames,
        i128::from(trigger_host_time_ns),
        i128::from(response_end),
    ) {
        let end = hand_frames[end_index];
        if !reservations.is_available(hand_frames, end_index)
            || keyboard_activity.overlaps_frame_window(hand_frames, end_index)
        {
            continue;
        }
        let Some(window) = tap_window_at(hand_frames, end_index) else {
            continue;
        };
        let contact_energy = HandDecoder::causal_contact_energies(&window)
            .into_iter()
            .max()
            .unwrap_or(0);
        if HandDecoder::common_motion_energy(&window) < thresholds.motion_energy
            || contact_energy >= thresholds.high_energy
            || HandDecoder::has_saturated_sample(&window)
        {
            continue;
        }
        reservations.reserve(hand_frames, end_index);
        return Some(end.record_ordinal);
    }
    None
}

fn select_training_peak(
    hand_frames: &[&IndexedFrame],
    position: PhysicalPosition,
    trigger_host_time_ns: u64,
    reservations: &mut WindowReservations,
) -> Option<(u64, i64)> {
    let mut best: Option<(usize, u64, f64, u64, i64)> = None;
    let trigger_host_time_ns = i128::from(trigger_host_time_ns);
    for end_index in candidate_end_indices(
        hand_frames,
        trigger_host_time_ns + i128::from(INTENT_WINDOW_START_NS),
        trigger_host_time_ns + i128::from(INTENT_WINDOW_END_NS),
    ) {
        let end = hand_frames[end_index];
        let raw_offset = i128::from(end.host_time_ns) - trigger_host_time_ns;
        let Ok(offset_ns) = i64::try_from(raw_offset) else {
            continue;
        };
        if !reservations.is_available(hand_frames, end_index) {
            continue;
        }
        let Some(window) = tap_window_at(hand_frames, end_index) else {
            continue;
        };
        let energy = HandDecoder::contact_energy(&window, position.finger);
        let distance = offset_ns.unsigned_abs();
        if best.is_none_or(|(_, _, best_energy, best_distance, _)| {
            energy > best_energy || (energy == best_energy && distance < best_distance)
        }) {
            best = Some((end_index, end.record_ordinal, energy, distance, offset_ns));
        }
    }
    let (end_index, record, _, _, offset) = best?;
    reservations.reserve(hand_frames, end_index);
    Some((record, offset))
}

fn candidate_end_indices(
    hand_frames: &[&IndexedFrame],
    start_host_time_ns: i128,
    end_host_time_ns: i128,
) -> std::ops::Range<usize> {
    let first = hand_frames
        .partition_point(|frame| i128::from(frame.host_time_ns) < start_host_time_ns)
        .max(crate::pipeline::WINDOW_FRAMES.saturating_sub(1));
    let end =
        hand_frames.partition_point(|frame| i128::from(frame.host_time_ns) <= end_host_time_ns);
    first..end
}

fn tap_window_at(hand_frames: &[&IndexedFrame], end_index: usize) -> Option<TapWindow> {
    TapWindow::new(
        window_frames_at(hand_frames, end_index)
            .iter()
            .map(|candidate| candidate.frame)
            .collect(),
    )
    .ok()
}

fn window_frames_at<'slice, 'frame>(
    hand_frames: &'slice [&'frame IndexedFrame],
    end_index: usize,
) -> &'slice [&'frame IndexedFrame] {
    let start = end_index + 1 - crate::pipeline::WINDOW_FRAMES;
    &hand_frames[start..=end_index]
}

#[derive(Debug)]
pub enum CalibrationCaptureError {
    TooLarge {
        actual: usize,
    },
    MatchingRuleTooLarge {
        actual: usize,
    },
    Json(String),
    Serialization(String),
    MatchingRuleJson(String),
    MatchingRuleSerialization(String),
    InvalidSchema,
    InvalidSha256(&'static str),
    InvalidMatchingRuleSchema,
    InvalidMatchingRuleSha256(&'static str),
    InvalidMatchingRule,
    IncompleteIdentity,
    MissingMatchingRule,
    MissingNeutralWindows,
    UnexpectedNeutralWindows,
    MatchingRuleManifestIdentityMismatch,
    MatchingRuleNeutralEvidenceMismatch,
    MissingNeutralContactEvidence(Hand),
    MissingSurfacePlan,
    SurfacePlanPurposeMismatch,
    InvalidSurfacePlanSha256,
    SurfacePlanIdentityMismatch,
    SurfacePlanArtifactMismatch,
    SurfacePlanTimingOverflow,
    SurfacePlan(SurfacePlanError),
    UnknownSurfacePrompt(String),
    DuplicateSurfacePrompt(String),
    ReorderedSurfacePrompt {
        expected: String,
        actual: String,
    },
    MissingSurfacePrompt(String),
    SurfacePromptKindMismatch(String),
    SurfacePromptTimingMismatch(String),
    SurfacePromptRevisionMismatch(String),
    SurfaceNullSampleMismatch(String),
    SurfaceResponseOverflow,
    NoSurfaceContact {
        position: PhysicalPosition,
        response_start_ns: u64,
        response_end_ns: u64,
    },
    WrongSurfaceHand {
        position: PhysicalPosition,
        observed: Hand,
    },
    WrongSurfaceFinger {
        position: PhysicalPosition,
        observed: Finger,
    },
    MultipleSurfaceContacts {
        position: PhysicalPosition,
        actual: usize,
    },
    KeyboardEventTimeRegression {
        previous_host_time_ns: u64,
        current_host_time_ns: u64,
    },
    KeyboardActivityOverlapsPassiveEvidence {
        start_host_time_ns: u64,
        end_host_time_ns: u64,
    },
    KeyboardEventDomainMismatch,
    KeyboardEventProvenanceMismatch,
    InvalidNullProvenance,
    InvalidSurfacePresentation,
    Manifest(ManifestError),
    Topology(ObservedTopologyError),
    Clock(ClockEpochError),
    ClockQualification(ClockEpochFitError),
    Session(SessionError),
    Evidence(CalibrationEvidenceError),
    DecodedRecord {
        record_ordinal: u64,
        message: String,
    },
    FrameCanonicalization {
        record_ordinal: u64,
        error: FrameCanonicalizationError,
    },
    RawFrameQuality {
        record_ordinal: u64,
        hand: Hand,
        reason: AbstainReason,
    },
    RawFrameStructure {
        record_ordinal: u64,
        hand: Hand,
        error: WindowError,
    },
    ManifestIdentityMismatch,
    SessionIdentityMismatch,
    ClockIdentityMismatch,
    UnexpectedControlRecord {
        kind: SessionRecordKind,
    },
    StreamIntegrity,
}

impl fmt::Display for CalibrationCaptureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge { actual } => write!(
                formatter,
                "calibration capture is {actual} bytes; maximum is {MAX_CALIBRATION_CAPTURE_BYTES}"
            ),
            Self::MatchingRuleTooLarge { actual } => write!(
                formatter,
                "calibration matching rule is {actual} bytes; maximum is {MAX_CALIBRATION_MATCHING_RULE_BYTES}"
            ),
            Self::Json(error) => write!(formatter, "calibration capture JSON is invalid: {error}"),
            Self::Serialization(error) => write!(
                formatter,
                "calibration capture serialization failed: {error}"
            ),
            Self::MatchingRuleJson(error) => write!(
                formatter,
                "calibration matching rule JSON is invalid: {error}"
            ),
            Self::MatchingRuleSerialization(error) => write!(
                formatter,
                "calibration matching rule serialization failed: {error}"
            ),
            Self::InvalidSchema => formatter.write_str("calibration capture schema is invalid"),
            Self::InvalidSha256(field) => write!(
                formatter,
                "calibration capture field {field} is not SHA-256"
            ),
            Self::InvalidMatchingRuleSchema => {
                formatter.write_str("calibration matching rule schema is invalid")
            }
            Self::InvalidMatchingRuleSha256(field) => write!(
                formatter,
                "calibration matching rule field {field} is not SHA-256"
            ),
            Self::InvalidMatchingRule => {
                formatter.write_str("calibration matching rule is incomplete or out of range")
            }
            Self::IncompleteIdentity => {
                formatter.write_str("calibration capture identity is incomplete")
            }
            Self::MissingMatchingRule => {
                formatter.write_str("held-out calibration capture has no frozen contact rule")
            }
            Self::MissingNeutralWindows => {
                formatter.write_str("fitting capture has no source-bound neutral windows")
            }
            Self::UnexpectedNeutralWindows => {
                formatter.write_str("held-out capture cannot redefine neutral windows")
            }
            Self::MatchingRuleManifestIdentityMismatch => formatter
                .write_str("calibration matching rule was learned for a different manifest"),
            Self::MatchingRuleNeutralEvidenceMismatch => formatter
                .write_str("calibration contact rule does not match immutable neutral evidence"),
            Self::MissingNeutralContactEvidence(hand) => write!(
                formatter,
                "calibration neutral evidence has no complete causal {hand:?} windows"
            ),
            Self::MissingSurfacePlan => {
                formatter.write_str("calibration capture has no canonical surface plan")
            }
            Self::SurfacePlanPurposeMismatch => formatter
                .write_str("calibration capture surface plan has the wrong evidence purpose"),
            Self::InvalidSurfacePlanSha256 => {
                formatter.write_str("calibration capture surface plan hash is invalid")
            }
            Self::SurfacePlanIdentityMismatch => {
                formatter.write_str("calibration capture does not bind the canonical surface plan")
            }
            Self::SurfacePlanArtifactMismatch => formatter
                .write_str("held-out calibration surface plan is bound to a different artifact"),
            Self::SurfacePlanTimingOverflow => {
                formatter.write_str("calibration surface plan exceeds the host clock range")
            }
            Self::SurfacePlan(error) => {
                write!(formatter, "calibration surface plan failed: {error}")
            }
            Self::UnknownSurfacePrompt(prompt_id) => {
                write!(formatter, "surface fact has unknown prompt ID {prompt_id}")
            }
            Self::DuplicateSurfacePrompt(prompt_id) => {
                write!(formatter, "surface fact repeats prompt ID {prompt_id}")
            }
            Self::ReorderedSurfacePrompt { expected, actual } => write!(
                formatter,
                "surface facts are out of plan order: expected {expected}, got {actual}"
            ),
            Self::MissingSurfacePrompt(prompt_id) => {
                write!(formatter, "surface facts omit prompt ID {prompt_id}")
            }
            Self::SurfacePromptKindMismatch(prompt_id) => write!(
                formatter,
                "surface fact kind does not match plan prompt {prompt_id}"
            ),
            Self::SurfacePromptTimingMismatch(prompt_id) => write!(
                formatter,
                "surface fact timing does not match plan prompt {prompt_id}"
            ),
            Self::SurfacePromptRevisionMismatch(prompt_id) => write!(
                formatter,
                "surface presentation revision is not increasing at prompt {prompt_id}"
            ),
            Self::SurfaceNullSampleMismatch(prompt_id) => write!(
                formatter,
                "surface null samples are not contiguous for prompt {prompt_id}"
            ),
            Self::SurfaceResponseOverflow => {
                formatter.write_str("surface response window exceeds the host clock range")
            }
            Self::NoSurfaceContact {
                position,
                response_start_ns,
                response_end_ns,
            } => write!(
                formatter,
                "no causal contact for {position:?} in {response_start_ns}..={response_end_ns} ns"
            ),
            Self::WrongSurfaceHand { position, observed } => write!(
                formatter,
                "first causal contact for {position:?} came from {observed:?}"
            ),
            Self::WrongSurfaceFinger { position, observed } => write!(
                formatter,
                "first causal contact for {position:?} came from {observed:?}"
            ),
            Self::MultipleSurfaceContacts { position, actual } => write!(
                formatter,
                "surface target {position:?} produced {actual} causal contacts"
            ),
            Self::KeyboardEventTimeRegression {
                previous_host_time_ns,
                current_host_time_ns,
            } => write!(
                formatter,
                "selected-keyboard host time regressed from {previous_host_time_ns} to {current_host_time_ns} ns"
            ),
            Self::KeyboardActivityOverlapsPassiveEvidence {
                start_host_time_ns,
                end_host_time_ns,
            } => write!(
                formatter,
                "selected-keyboard activity overlaps passive evidence in {start_host_time_ns}..={end_host_time_ns} ns"
            ),
            Self::KeyboardEventDomainMismatch => {
                formatter.write_str("captured keyboard event must use the keyboard evidence domain")
            }
            Self::KeyboardEventProvenanceMismatch => formatter.write_str(
                "keyboard candidates are fitting-only; state-only events must use the capture purpose's role and keyboard domain",
            ),
            Self::InvalidNullProvenance => formatter.write_str(
                "threshold capture accepts quiet null windows, not ordinary-motion windows",
            ),
            Self::InvalidSurfacePresentation => {
                formatter.write_str("surface presentation receipt is missing or out of order")
            }
            Self::Manifest(error) => {
                write!(formatter, "calibration capture manifest failed: {error}")
            }
            Self::Topology(error) => {
                write!(formatter, "calibration capture topology failed: {error}")
            }
            Self::Clock(error) => write!(formatter, "calibration capture clocks failed: {error}"),
            Self::ClockQualification(error) => write!(formatter, "calibration capture clock qualification failed: {error}"),
            Self::Session(error) => {
                write!(formatter, "calibration capture session failed: {error}")
            }
            Self::Evidence(error) => write!(
                formatter,
                "calibration evidence construction failed: {error}"
            ),
            Self::DecodedRecord {
                record_ordinal,
                message,
            } => write!(
                formatter,
                "calibration capture record {record_ordinal} failed to decode: {message}"
            ),
            Self::FrameCanonicalization {
                record_ordinal,
                error,
            } => write!(
                formatter,
                "calibration capture record {record_ordinal} cannot enter canonical sensor coordinates: {error}"
            ),
            Self::RawFrameQuality {
                record_ordinal,
                hand,
                reason,
            } => write!(
                formatter,
                "calibration capture record {record_ordinal} for {hand:?} has invalid raw acquisition quality: {reason:?}"
            ),
            Self::RawFrameStructure {
                record_ordinal,
                hand,
                error,
            } => write!(
                formatter,
                "calibration capture record {record_ordinal} for {hand:?} has invalid raw timing: {error}"
            ),
            Self::ManifestIdentityMismatch => {
                formatter.write_str("calibration capture does not bind the manifest")
            }
            Self::SessionIdentityMismatch => {
                formatter.write_str("calibration capture does not bind the sensor session")
            }
            Self::ClockIdentityMismatch => {
                formatter.write_str("calibration capture does not bind the clock epochs")
            }
            Self::UnexpectedControlRecord { kind } => write!(
                formatter,
                "calibration capture session contains a {kind:?} Gate 0A control record"
            ),
            Self::StreamIntegrity => {
                formatter.write_str("calibration capture failed the lossless stream-integrity gate")
            }
        }
    }
}

impl Error for CalibrationCaptureError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::ClockQualification(error) => Some(error),
            Self::RawFrameStructure { error, .. } => Some(error),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{layout::all_slots, simulation::SyntheticConfig};

    fn rule() -> CalibrationMatchingRule {
        CalibrationMatchingRule {
            schema: CALIBRATION_MATCHING_RULE_SCHEMA.to_owned(),
            acquisition_manifest_sha256: format_sha256(&[0x11; 32]),
            neutral_windows_sha256: format_sha256(&[0x22; 32]),
            neutral_source_sha256: format_sha256(&[0x33; 32]),
            feature_schema: CONTACT_FEATURE_SCHEMA.to_owned(),
            thresholds: [
                HandContactThresholds {
                    hand: Hand::Left,
                    low_energy: CONTACT_LOW_ENERGY_FLOOR,
                    high_energy: CONTACT_HIGH_ENERGY_FLOOR,
                    motion_energy: MOTION_ENERGY_FLOOR,
                },
                HandContactThresholds {
                    hand: Hand::Right,
                    low_energy: CONTACT_LOW_ENERGY_FLOOR,
                    high_energy: CONTACT_HIGH_ENERGY_FLOOR,
                    motion_energy: MOTION_ENERGY_FLOOR,
                },
            ],
            debounce_windows: CONTACT_DEBOUNCE_WINDOWS,
            rearm_windows: CONTACT_REARM_WINDOWS,
            response_start_ns: CONTACT_RESPONSE_START_NS,
            response_end_ns: CONTACT_RESPONSE_END_NS,
        }
    }

    fn position(hand: Hand, finger: Finger) -> PhysicalPosition {
        all_slots()
            .iter()
            .find(|slot| slot.position.hand == hand && slot.position.finger == finger)
            .expect("fixture hand/finger has a position")
            .position
    }

    fn receipt(acknowledged_host_time_ns: u64) -> SurfacePresentationReceipt {
        SurfacePresentationReceipt::new(
            acknowledged_host_time_ns - 2,
            acknowledged_host_time_ns - 1,
            acknowledged_host_time_ns,
            1,
        )
        .expect("fixture receipt is ordered")
    }

    fn contact(
        hand: Hand,
        finger: Finger,
        host_time_ns: u64,
        window_end_record: u64,
    ) -> CausalContactEvent {
        CausalContactEvent {
            hand,
            finger,
            host_time_ns,
            window_end_record,
        }
    }

    #[test]
    fn rendered_receipt_opens_a_self_paced_gate_but_sensor_contact_anchors_the_label() {
        let acknowledged = 1_000_000_000;
        let contact_time = acknowledged + 180_000_000;
        let target = position(Hand::Left, Finger::Index);
        let event = associate_prompted_contact(
            target,
            receipt(acknowledged),
            &[contact(Hand::Left, Finger::Index, contact_time, 41)],
            |_| true,
            &rule(),
        )
        .expect("a delayed causal contact is accepted");
        assert_eq!(event.host_time_ns, contact_time);
        assert_ne!(event.host_time_ns, acknowledged);
    }

    #[test]
    fn pre_cue_contact_is_ignored_without_hiding_the_first_post_cue_contact() {
        let acknowledged = 2_000_000_000;
        let target = position(Hand::Left, Finger::Middle);
        let selected = associate_prompted_contact(
            target,
            receipt(acknowledged),
            &[
                contact(Hand::Left, Finger::Middle, acknowledged - 1, 51),
                contact(Hand::Left, Finger::Middle, acknowledged + 500_000_000, 52),
            ],
            |_| true,
            &rule(),
        )
        .expect("only the causal post-cue event enters the response");
        assert_eq!(selected.window_end_record, 52);
    }

    #[test]
    fn first_event_rule_reports_wrong_hand_wrong_finger_no_event_and_multiple_events() {
        let acknowledged = 3_000_000_000;
        let target = position(Hand::Left, Finger::Index);
        let frozen = rule();
        assert!(matches!(
            associate_prompted_contact(target, receipt(acknowledged), &[], |_| true, &frozen),
            Err(CalibrationCaptureError::NoSurfaceContact { .. })
        ));
        assert!(matches!(
            associate_prompted_contact(
                target,
                receipt(acknowledged),
                &[
                    contact(Hand::Right, Finger::Index, acknowledged + 100, 61),
                    contact(Hand::Left, Finger::Index, acknowledged + 200, 62),
                ],
                |_| true,
                &frozen,
            ),
            Err(CalibrationCaptureError::WrongSurfaceHand { .. })
        ));
        assert!(matches!(
            associate_prompted_contact(
                target,
                receipt(acknowledged),
                &[
                    contact(Hand::Left, Finger::Middle, acknowledged + 100, 63),
                    contact(Hand::Left, Finger::Index, acknowledged + 200, 64),
                ],
                |_| true,
                &frozen,
            ),
            Err(CalibrationCaptureError::WrongSurfaceFinger { .. })
        ));
        assert!(matches!(
            associate_prompted_contact(
                target,
                receipt(acknowledged),
                &[
                    contact(Hand::Left, Finger::Index, acknowledged + 100, 65),
                    contact(Hand::Left, Finger::Index, acknowledged + 200, 66),
                ],
                |_| true,
                &frozen,
            ),
            Err(CalibrationCaptureError::MultipleSurfaceContacts { actual: 2, .. })
        ));
    }

    #[test]
    fn target_identity_cannot_select_past_the_first_causal_event() {
        let acknowledged = 4_000_000_000;
        let target = position(Hand::Left, Finger::Pinky);
        assert!(matches!(
            associate_prompted_contact(
                target,
                receipt(acknowledged),
                &[
                    contact(Hand::Left, Finger::Index, acknowledged + 100, 71),
                    contact(Hand::Left, Finger::Pinky, acknowledged + 200, 72),
                ],
                |_| true,
                &rule(),
            ),
            Err(CalibrationCaptureError::WrongSurfaceFinger {
                observed: Finger::Index,
                ..
            })
        ));
    }

    #[test]
    fn contacts_after_the_bounded_gate_cannot_change_an_accepted_association() {
        let acknowledged = 5_000_000_000;
        let target = position(Hand::Right, Finger::Ring);
        let accepted = contact(Hand::Right, Finger::Ring, acknowledged + 100, 81);
        let first = associate_prompted_contact(
            target,
            receipt(acknowledged),
            &[accepted],
            |_| true,
            &rule(),
        )
        .expect("first association is accepted");
        let with_future = associate_prompted_contact(
            target,
            receipt(acknowledged),
            &[
                accepted,
                contact(
                    Hand::Right,
                    Finger::Ring,
                    acknowledged + CONTACT_RESPONSE_END_NS + 1,
                    82,
                ),
            ],
            |_| true,
            &rule(),
        )
        .expect("post-gate future data cannot change the association");
        assert_eq!(first, with_future);
    }

    fn indexed(
        config: &SyntheticConfig,
        trials: &[u64],
        tap: Option<PhysicalPosition>,
    ) -> Vec<IndexedFrame> {
        let mut frames = Vec::new();
        for &trial in trials {
            let window = if tap.is_some_and(|_| trial == trials[trials.len() / 2]) {
                config.tap_window(tap.expect("checked tap exists"), trial)
            } else {
                config.idle_window(trial)
            };
            for frame in window.frames() {
                frames.push(IndexedFrame {
                    record_ordinal: u64::try_from(frames.len()).expect("fixture length fits u64"),
                    host_time_ns: 1_000_000_000 + frame.device_time_us * 1_000,
                    frame: *frame,
                });
            }
        }
        frames
    }

    #[test]
    fn causal_contact_emission_is_invariant_to_future_frames() {
        let config = SyntheticConfig::new(Hand::Left, 0x5151)
            .with_noise(0)
            .with_shared_motion(0);
        let target = position(Hand::Left, Finger::Index);
        let prefix = indexed(&config, &[0, 1], Some(target));
        let mut extended = prefix.clone();
        let future = indexed(&config, &[2], None);
        let start_record = u64::try_from(extended.len()).expect("fixture length fits u64");
        extended.extend(future.into_iter().enumerate().map(|(index, mut frame)| {
            frame.record_ordinal = start_record + u64::try_from(index).expect("index fits u64");
            frame
        }));
        let prefix_events = detect_causal_contacts(&prefix, &rule());
        let extended_events = detect_causal_contacts(&extended, &rule());
        assert_eq!(prefix_events, extended_events);
        assert_eq!(prefix_events.len(), 1);
    }

    #[test]
    fn scheduled_stillness_is_not_counted_as_ordinary_motion() {
        let still_config = SyntheticConfig::new(Hand::Left, 0x6161)
            .with_noise(0)
            .with_shared_motion(0);
        let motion_config = still_config.with_shared_motion(2_000);
        let still = indexed(&still_config, &[0], None);
        let motion = indexed(&motion_config, &[0], None);
        let frozen = rule();
        let thresholds = thresholds_for_hand(&frozen, Hand::Left);
        assert!(
            select_measured_motion_window_end(
                &still.iter().collect::<Vec<_>>(),
                still[0].host_time_ns,
                thresholds,
                &mut WindowReservations::default(),
                &SelectedKeyboardActivity::default(),
            )
            .is_none()
        );
        assert!(
            select_measured_motion_window_end(
                &motion.iter().collect::<Vec<_>>(),
                motion[0].host_time_ns,
                thresholds,
                &mut WindowReservations::default(),
                &SelectedKeyboardActivity::default(),
            )
            .is_some()
        );
    }

    #[test]
    fn target_truth_windows_reserve_all_source_records_against_later_targets() {
        let config = SyntheticConfig::new(Hand::Left, 0x7171)
            .with_noise(0)
            .with_shared_motion(0);
        let frames = indexed(&config, &[0, 1], None);
        let hand_frames = frames.iter().collect::<Vec<_>>();
        let target = position(Hand::Left, Finger::Index);
        let first_contact = contact(
            Hand::Left,
            Finger::Index,
            frames[15].host_time_ns,
            frames[15].record_ordinal,
        );
        let overlapping_contact = contact(
            Hand::Left,
            Finger::Index,
            frames[16].host_time_ns,
            frames[16].record_ordinal,
        );
        let mut reservations = WindowReservations::default();
        let selected = associate_prompted_contact(
            target,
            receipt(first_contact.host_time_ns),
            &[first_contact],
            |candidate| {
                reservations.record_window_is_available(&hand_frames, candidate.window_end_record)
            },
            &rule(),
        )
        .expect("the first target owns its complete truth window");
        assert!(reservations.reserve_record_window(&hand_frames, selected.window_end_record));
        assert_eq!(
            reservations.frame_records.len(),
            crate::pipeline::WINDOW_FRAMES
        );

        assert!(matches!(
            associate_prompted_contact(
                target,
                receipt(overlapping_contact.host_time_ns),
                &[overlapping_contact],
                |candidate| reservations
                    .record_window_is_available(&hand_frames, candidate.window_end_record),
                &rule(),
            ),
            Err(CalibrationCaptureError::NoSurfaceContact { .. })
        ));
    }

    #[test]
    fn target_cannot_reuse_a_threshold_or_null_parameter_window() {
        let config = SyntheticConfig::new(Hand::Left, 0x7272)
            .with_noise(0)
            .with_shared_motion(0);
        let frames = indexed(&config, &[0, 1], None);
        let hand_frames = frames.iter().collect::<Vec<_>>();
        let mut reservations = WindowReservations::default();
        assert_eq!(
            select_causal_window_end(
                &hand_frames,
                frames[15].host_time_ns,
                &mut reservations,
                &SelectedKeyboardActivity::default(),
            ),
            Some(frames[15].record_ordinal),
            "the independently labelled null/threshold path owns the first window"
        );
        let target = position(Hand::Left, Finger::Middle);
        let overlapping_contact = contact(
            Hand::Left,
            Finger::Middle,
            frames[16].host_time_ns,
            frames[16].record_ordinal,
        );
        assert!(matches!(
            associate_prompted_contact(
                target,
                receipt(overlapping_contact.host_time_ns),
                &[overlapping_contact],
                |candidate| reservations
                    .record_window_is_available(&hand_frames, candidate.window_end_record),
                &rule(),
            ),
            Err(CalibrationCaptureError::NoSurfaceContact { .. })
        ));
    }

    #[test]
    fn capture_gate_rejects_gate0_controls_before_generic_integrity() {
        for (summary, expected) in [
            (
                crate::session::SessionSummary {
                    quiesce_requests: 1,
                    ..crate::session::SessionSummary::default()
                },
                SessionRecordKind::QuiesceRequest,
            ),
            (
                crate::session::SessionSummary {
                    quiesce_receipts: 1,
                    ..crate::session::SessionSummary::default()
                },
                SessionRecordKind::QuiesceReceipt,
            ),
        ] {
            assert!(matches!(
                reject_control_records(&summary),
                Err(CalibrationCaptureError::UnexpectedControlRecord { kind }) if kind == expected
            ));
        }
    }
}
