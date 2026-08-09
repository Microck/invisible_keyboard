//! Pure guided-calibration state, coverage, and stopping rules.
//!
//! Device discovery, Linux key-event capture, signal-window matching, model
//! fitting, persistence, and browser rendering remain adapters around this
//! module. Keeping the coordinator pure makes every user-visible transition
//! replayable before the first physical rig exists.

use std::{collections::BTreeSet, error::Error, fmt};

use crate::layout::{
    PHYSICAL_POSITION_COUNT, PhysicalPosition, all_slots, keyboard_warm_start_eligible,
    position_index,
};
use crate::validation::{ValidationEvidenceError, ValidationReport};

/// Initial accepted examples for every keyboard-addressable position.
pub const KEYBOARD_EXAMPLES_PER_POSITION: u8 = 8;

/// Initial prompted taps for every passive-surface position.
pub const SURFACE_EXAMPLES_PER_POSITION: u8 = 3;

/// Maximum accepted examples for one position before setup stops.
pub const MAX_EXAMPLES_PER_POSITION: u8 = 20;

/// Provisional full-setup time cap from the calibration contract.
/// Finite engineering cap for keyboard warm-start plus passive calibration.
///
/// The 20-minute value is provisional until hardware trials establish real
/// response-time and retry distributions.
pub const SETUP_TIME_CAP_SECONDS: u32 = 1_200;

const PASS_KEYBOARD_INTENT_SEPARATION_BASIS_POINTS: u16 = 9_000;
const PASS_KEYBOARD_FINGER_AGREEMENT_BASIS_POINTS: u16 = 8_000;
const PASS_MACRO_F1_BASIS_POINTS: u16 = 9_000;
const PASS_MINIMUM_RECALL_BASIS_POINTS: u16 = 8_000;
const MINIMUM_IMPROVEMENT_BASIS_POINTS: u16 = 100;
const PLATEAU_BLOCK_LIMIT: u8 = 3;

/// Stable identity for an immutable collection or validation block.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct BlockId(u32);

impl BlockId {
    /// Construct an opaque block identifier assigned before collection.
    #[must_use]
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn value(self) -> u32 {
        self.0
    }
}

/// Immutable model identity and the sessions that may not enter validation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LockedArtifactIdentity {
    fingerprint: [u8; 32],
    manifest_sha256: [u8; 32],
    source_sessions: Vec<[u8; 32]>,
}

impl LockedArtifactIdentity {
    pub fn new(
        fingerprint: [u8; 32],
        manifest_sha256: [u8; 32],
        source_sessions: Vec<[u8; 32]>,
    ) -> Result<Self, ArtifactIdentityError> {
        if source_sessions.is_empty() {
            return Err(ArtifactIdentityError::MissingSourceSessions);
        }
        let unique = source_sessions.iter().copied().collect::<BTreeSet<_>>();
        if unique.len() != source_sessions.len() {
            return Err(ArtifactIdentityError::DuplicateSourceSession);
        }
        Ok(Self {
            fingerprint,
            manifest_sha256,
            source_sessions,
        })
    }

    #[must_use]
    pub const fn fingerprint(&self) -> &[u8; 32] {
        &self.fingerprint
    }

    #[must_use]
    pub const fn manifest_sha256(&self) -> &[u8; 32] {
        &self.manifest_sha256
    }

    #[must_use]
    pub fn source_sessions(&self) -> &[[u8; 32]] {
        &self.source_sessions
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArtifactIdentityError {
    MissingSourceSessions,
    DuplicateSourceSession,
}

impl fmt::Display for ArtifactIdentityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingSourceSessions => {
                formatter.write_str("locked artifact has no source sessions")
            }
            Self::DuplicateSourceSession => {
                formatter.write_str("locked artifact repeats a source session")
            }
        }
    }
}

impl Error for ArtifactIdentityError {}

/// Calibration evidence domains that must remain distinct.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CalibrationDomain {
    Keyboard,
    PassiveSurface,
}

/// Only the one frozen logical layout supported by version 1.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KeyboardLayout {
    UsQwerty,
    Unsupported,
}

/// One state in the canonical setup flow.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CalibrationPhase {
    Disconnected,
    HardwareCheck,
    KeyboardIdentify,
    NeutralPose,
    KeyboardCollect,
    KeyboardValidate,
    SurfaceCollect,
    FitLockedArtifact,
    SurfaceValidate,
    ValidationPassed,
    NeedsHardwareAttention,
    CalibrationFailed,
    Aborted,
}

impl CalibrationPhase {
    const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::ValidationPassed
                | Self::NeedsHardwareAttention
                | Self::CalibrationFailed
                | Self::Aborted
        )
    }
}

/// Named setup outcomes that cannot be repaired by silently collecting forever.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CalibrationFailure {
    HardwareFault,
    UnsupportedLayout,
    SurfaceValidation,
    ValidationPlateau,
    ExampleCap,
    TimeCap,
}

/// Why one raw event remains evidence but does not advance coverage.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RejectedExampleReason {
    WrongKeyboard,
    Repeat,
    OverlappingPress,
    MissingSensor,
    AcquisitionFault,
    TimingSkew,
    UnmatchedImpulse,
    ActingFingerMismatch,
}

/// Selection-block checks that can run before passive-surface collection.
///
/// These values describe keyboard-labelled intent and acting-finger evidence.
/// They are not key-identity accuracy and cannot authorize live use.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct KeyboardPrecheckMetrics {
    pub intent_separation_basis_points: u16,
    pub finger_agreement_basis_points: u16,
}

impl KeyboardPrecheckMetrics {
    /// Construct metrics in the inclusive range 0 through 10,000.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationMetricError`] for an impossible percentage.
    pub const fn new(
        intent_separation_basis_points: u16,
        finger_agreement_basis_points: u16,
    ) -> Result<Self, ValidationMetricError> {
        if intent_separation_basis_points > 10_000 || finger_agreement_basis_points > 10_000 {
            return Err(ValidationMetricError);
        }
        Ok(Self {
            intent_separation_basis_points,
            finger_agreement_basis_points,
        })
    }

    pub(crate) const fn passes(self) -> bool {
        self.intent_separation_basis_points >= PASS_KEYBOARD_INTENT_SEPARATION_BASIS_POINTS
            && self.finger_agreement_basis_points >= PASS_KEYBOARD_FINGER_AGREEMENT_BASIS_POINTS
    }

    const fn progress_score(self) -> u16 {
        if self.intent_separation_basis_points < self.finger_agreement_basis_points {
            self.intent_separation_basis_points
        } else {
            self.finger_agreement_basis_points
        }
    }
}

/// Point-estimate key-identity metrics derived from a count-level report.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HeldOutValidationMetrics {
    pub macro_f1_basis_points: u16,
    pub minimum_recall_basis_points: u16,
}

impl HeldOutValidationMetrics {
    pub const fn new(
        macro_f1_basis_points: u16,
        minimum_recall_basis_points: u16,
    ) -> Result<Self, ValidationMetricError> {
        if macro_f1_basis_points > 10_000 || minimum_recall_basis_points > 10_000 {
            return Err(ValidationMetricError);
        }
        Ok(Self {
            macro_f1_basis_points,
            minimum_recall_basis_points,
        })
    }

    pub(crate) const fn passes(self) -> bool {
        self.macro_f1_basis_points >= PASS_MACRO_F1_BASIS_POINTS
            && self.minimum_recall_basis_points >= PASS_MINIMUM_RECALL_BASIS_POINTS
    }
}

/// A validation score outside the percentage range.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ValidationMetricError;

impl fmt::Display for ValidationMetricError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("validation metrics must be between 0 and 10000 basis points")
    }
}

impl Error for ValidationMetricError {}

/// Pure API misuse or an invariant violation in a replayed setup.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CoordinatorError {
    InvalidTransition {
        phase: CalibrationPhase,
        action: &'static str,
    },
    UnknownPosition(PhysicalPosition),
    PositionNotAddressable(PhysicalPosition),
    BlockOverlap(BlockId),
    DuplicateValidationBlock(BlockId),
    CoverageIncomplete(CalibrationDomain),
    InvalidWeakPosition(PhysicalPosition),
    InvalidArtifactIdentity(ArtifactIdentityError),
    InvalidValidationEvidence(ValidationEvidenceError),
    ElapsedTimeRegression {
        previous_seconds: u32,
        actual_seconds: u32,
    },
}

impl fmt::Display for CoordinatorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidTransition { phase, action } => {
                write!(formatter, "cannot {action} during {phase:?}")
            }
            Self::UnknownPosition(position) => {
                write!(formatter, "position {position:?} is not in the manifest")
            }
            Self::PositionNotAddressable(position) => write!(
                formatter,
                "position {position:?} has no physical-keyboard label"
            ),
            Self::BlockOverlap(block) => {
                write!(
                    formatter,
                    "block {block:?} cannot be both training and validation"
                )
            }
            Self::DuplicateValidationBlock(block) => {
                write!(formatter, "validation block {block:?} was already scored")
            }
            Self::CoverageIncomplete(domain) => {
                write!(formatter, "{domain:?} coverage is incomplete")
            }
            Self::InvalidWeakPosition(position) => {
                write!(
                    formatter,
                    "weak position {position:?} is not keyboard-addressable"
                )
            }
            Self::InvalidArtifactIdentity(error) => error.fmt(formatter),
            Self::InvalidValidationEvidence(error) => error.fmt(formatter),
            Self::ElapsedTimeRegression {
                previous_seconds,
                actual_seconds,
            } => write!(
                formatter,
                "setup elapsed time regressed from {previous_seconds} to {actual_seconds} seconds"
            ),
        }
    }
}

impl Error for CoordinatorError {}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct PositionCoverage {
    accepted: u8,
    required: u8,
    first_block: Option<BlockId>,
    second_block: Option<BlockId>,
}

impl PositionCoverage {
    fn observe_block(&mut self, block: BlockId) {
        match (self.first_block, self.second_block) {
            (None, _) => self.first_block = Some(block),
            (Some(first), None) if first != block => self.second_block = Some(block),
            _ => {}
        }
    }

    const fn keyboard_complete(self) -> bool {
        self.required == 0 || (self.accepted >= self.required && self.second_block.is_some())
    }

    const fn surface_complete(self) -> bool {
        self.accepted >= self.required
    }

    const fn keyboard_deficit(self) -> u16 {
        if self.required == 0 || self.keyboard_complete() {
            return 0;
        }
        let count_deficit = self.required.saturating_sub(self.accepted);
        let block_deficit = if self.second_block.is_none() {
            self.required
        } else {
            0
        };
        count_deficit as u16 + block_deficit as u16
    }

    const fn surface_deficit(self) -> u16 {
        self.required.saturating_sub(self.accepted) as u16
    }
}

/// Separate setup progress values. None is an accuracy claim.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CalibrationProgress {
    pub phase: CalibrationPhase,
    pub keyboard_accepted: u32,
    pub keyboard_required: u32,
    pub surface_accepted: u32,
    pub surface_required: u32,
    pub rejected_examples: u32,
    pub elapsed_seconds: u32,
}

/// Deterministic coordinator for one fitting and held-out calibration.
#[derive(Clone, Debug)]
pub struct CalibrationCoordinator {
    phase: CalibrationPhase,
    failure: Option<CalibrationFailure>,
    keyboard: [PositionCoverage; PHYSICAL_POSITION_COUNT],
    surface: [PositionCoverage; PHYSICAL_POSITION_COUNT],
    training_blocks: BTreeSet<BlockId>,
    validation_blocks: BTreeSet<BlockId>,
    rejected_examples: u32,
    elapsed_seconds: u32,
    previous_keyboard_precheck_score: Option<u16>,
    plateau_blocks: u8,
    artifact_identity: Option<LockedArtifactIdentity>,
    surface_validation_block: Option<BlockId>,
}

impl Default for CalibrationCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

impl CalibrationCoordinator {
    /// Construct a disconnected, empty setup.
    #[must_use]
    pub fn new() -> Self {
        let mut keyboard = [PositionCoverage::default(); PHYSICAL_POSITION_COUNT];
        let mut surface = [PositionCoverage::default(); PHYSICAL_POSITION_COUNT];
        for (index, slot) in all_slots().iter().enumerate() {
            if keyboard_warm_start_eligible(slot.position) {
                keyboard[index].required = KEYBOARD_EXAMPLES_PER_POSITION;
            }
            surface[index].required = SURFACE_EXAMPLES_PER_POSITION;
        }
        Self {
            phase: CalibrationPhase::Disconnected,
            failure: None,
            keyboard,
            surface,
            training_blocks: BTreeSet::new(),
            validation_blocks: BTreeSet::new(),
            rejected_examples: 0,
            elapsed_seconds: 0,
            previous_keyboard_precheck_score: None,
            plateau_blocks: 0,
            artifact_identity: None,
            surface_validation_block: None,
        }
    }

    /// Return the one current setup state.
    #[must_use]
    pub const fn phase(&self) -> CalibrationPhase {
        self.phase
    }

    /// Return the terminal failure, if any.
    #[must_use]
    pub const fn failure(&self) -> Option<CalibrationFailure> {
        self.failure
    }

    /// Return the immutable fingerprint after model fitting locks it.
    #[must_use]
    pub const fn artifact_fingerprint(&self) -> Option<&[u8; 32]> {
        match &self.artifact_identity {
            Some(identity) => Some(identity.fingerprint()),
            None => None,
        }
    }

    /// Return independent coverage and state progress.
    #[must_use]
    pub fn progress(&self) -> CalibrationProgress {
        let (keyboard_accepted, keyboard_required) = progress_totals(&self.keyboard);
        let (surface_accepted, surface_required) = progress_totals(&self.surface);
        CalibrationProgress {
            phase: self.phase,
            keyboard_accepted,
            keyboard_required,
            surface_accepted,
            surface_required,
            rejected_examples: self.rejected_examples,
            elapsed_seconds: self.elapsed_seconds,
        }
    }

    /// Enter the hardware check from the disconnected screen.
    pub fn connect(&mut self) -> Result<(), CoordinatorError> {
        self.require_phase(CalibrationPhase::Disconnected, "connect")?;
        self.phase = CalibrationPhase::HardwareCheck;
        Ok(())
    }

    /// Accept one complete healthy topology check.
    pub fn accept_hardware_check(&mut self) -> Result<(), CoordinatorError> {
        self.require_phase(CalibrationPhase::HardwareCheck, "accept hardware check")?;
        self.phase = CalibrationPhase::KeyboardIdentify;
        Ok(())
    }

    /// Freeze the logical keyboard layout used for warm-start labels.
    pub fn identify_keyboard(&mut self, layout: KeyboardLayout) -> Result<(), CoordinatorError> {
        self.require_phase(CalibrationPhase::KeyboardIdentify, "identify keyboard")?;
        match layout {
            KeyboardLayout::UsQwerty => self.phase = CalibrationPhase::NeutralPose,
            KeyboardLayout::Unsupported => {
                self.stop(
                    CalibrationPhase::CalibrationFailed,
                    CalibrationFailure::UnsupportedLayout,
                );
            }
        }
        Ok(())
    }

    /// Accept a stable neutral-pose block and start keyboard collection.
    pub fn accept_neutral_pose(&mut self) -> Result<(), CoordinatorError> {
        self.require_phase(CalibrationPhase::NeutralPose, "accept neutral pose")?;
        self.phase = CalibrationPhase::KeyboardCollect;
        Ok(())
    }

    /// Return the deterministic highest-deficit keyboard position.
    #[must_use]
    pub fn next_keyboard_position(&self) -> Option<PhysicalPosition> {
        next_position(&self.keyboard, PositionCoverage::keyboard_deficit)
    }

    /// Return the deterministic highest-deficit passive-surface position.
    #[must_use]
    pub fn next_surface_position(&self) -> Option<PhysicalPosition> {
        next_position(&self.surface, PositionCoverage::surface_deficit)
    }

    /// Accept one already quality-checked and one-to-one matched example.
    ///
    /// # Errors
    ///
    /// Returns an invariant error for the wrong phase/domain, unknown or
    /// keyboard-unaddressable position, or a train/validation block overlap.
    pub fn accept_example(
        &mut self,
        domain: CalibrationDomain,
        position: PhysicalPosition,
        block: BlockId,
    ) -> Result<(), CoordinatorError> {
        let expected_phase = match domain {
            CalibrationDomain::Keyboard => CalibrationPhase::KeyboardCollect,
            CalibrationDomain::PassiveSurface => CalibrationPhase::SurfaceCollect,
        };
        self.require_phase(expected_phase, "accept example")?;
        let index = position_index(position).ok_or(CoordinatorError::UnknownPosition(position))?;
        if domain == CalibrationDomain::Keyboard && !keyboard_warm_start_eligible(position) {
            return Err(CoordinatorError::PositionNotAddressable(position));
        }
        self.register_training_block(block)?;

        let coverage = match domain {
            CalibrationDomain::Keyboard => &mut self.keyboard[index],
            CalibrationDomain::PassiveSurface => &mut self.surface[index],
        };
        if coverage.accepted >= MAX_EXAMPLES_PER_POSITION {
            self.stop(
                CalibrationPhase::CalibrationFailed,
                CalibrationFailure::ExampleCap,
            );
            return Ok(());
        }
        coverage.accepted += 1;
        coverage.observe_block(block);
        Ok(())
    }

    /// Retain a rejected raw event without changing coverage.
    pub fn reject_example(
        &mut self,
        _reason: RejectedExampleReason,
    ) -> Result<(), CoordinatorError> {
        self.reject_examples(1)
    }

    /// Retain an evidence-side aggregate of rejected events without changing
    /// coverage. Replay sidecars store a bounded count rather than repeating
    /// one state-machine event per rejection.
    pub fn reject_examples(&mut self, count: u32) -> Result<(), CoordinatorError> {
        if !matches!(
            self.phase,
            CalibrationPhase::KeyboardCollect | CalibrationPhase::SurfaceCollect
        ) {
            return Err(CoordinatorError::InvalidTransition {
                phase: self.phase,
                action: "reject example",
            });
        }
        self.rejected_examples = self.rejected_examples.saturating_add(count);
        Ok(())
    }

    /// Freeze one disjoint keyboard-labelled signal-precheck block.
    pub fn begin_keyboard_precheck(&mut self, block: BlockId) -> Result<(), CoordinatorError> {
        self.require_phase(CalibrationPhase::KeyboardCollect, "begin keyboard precheck")?;
        if !self
            .keyboard
            .iter()
            .copied()
            .all(PositionCoverage::keyboard_complete)
        {
            return Err(CoordinatorError::CoverageIncomplete(
                CalibrationDomain::Keyboard,
            ));
        }
        self.register_validation_block(block)?;
        self.phase = CalibrationPhase::KeyboardValidate;
        Ok(())
    }

    /// Apply the keyboard-labelled signal precheck and target named weak keys.
    ///
    /// This gate checks intent separation and acting-finger agreement. The
    /// locked passive-surface report is the only key-identity readiness gate.
    pub fn submit_keyboard_precheck(
        &mut self,
        metrics: KeyboardPrecheckMetrics,
        weak_positions: &[PhysicalPosition],
    ) -> Result<(), CoordinatorError> {
        self.require_phase(
            CalibrationPhase::KeyboardValidate,
            "submit keyboard precheck",
        )?;
        if metrics.passes() {
            self.previous_keyboard_precheck_score = Some(metrics.progress_score());
            self.plateau_blocks = 0;
            self.phase = CalibrationPhase::SurfaceCollect;
            return Ok(());
        }

        let target_indexes = if weak_positions.is_empty() {
            self.keyboard
                .iter()
                .enumerate()
                .filter_map(|(index, coverage)| (coverage.required > 0).then_some(index))
                .collect::<BTreeSet<_>>()
        } else {
            weak_positions
                .iter()
                .copied()
                .map(|position| {
                    position_index(position)
                        .filter(|&index| self.keyboard[index].required > 0)
                        .ok_or(CoordinatorError::InvalidWeakPosition(position))
                })
                .collect::<Result<BTreeSet<_>, _>>()?
        };

        self.observe_keyboard_improvement(metrics.progress_score());
        if self.plateau_blocks >= PLATEAU_BLOCK_LIMIT {
            self.stop(
                CalibrationPhase::CalibrationFailed,
                CalibrationFailure::ValidationPlateau,
            );
            return Ok(());
        }

        if target_indexes
            .iter()
            .any(|&index| !can_raise_requirement(self.keyboard[index]))
        {
            self.stop(
                CalibrationPhase::CalibrationFailed,
                CalibrationFailure::ExampleCap,
            );
            return Ok(());
        }
        for index in target_indexes {
            self.keyboard[index].required += 1;
        }
        self.phase = CalibrationPhase::KeyboardCollect;
        Ok(())
    }

    /// Freeze all passive training and reserve a disjoint validation block.
    pub fn begin_artifact_fit(
        &mut self,
        validation_block: BlockId,
    ) -> Result<(), CoordinatorError> {
        self.require_phase(CalibrationPhase::SurfaceCollect, "begin artifact fit")?;
        if !self
            .surface
            .iter()
            .copied()
            .all(PositionCoverage::surface_complete)
        {
            return Err(CoordinatorError::CoverageIncomplete(
                CalibrationDomain::PassiveSurface,
            ));
        }
        self.register_validation_block(validation_block)?;
        self.surface_validation_block = Some(validation_block);
        self.phase = CalibrationPhase::FitLockedArtifact;
        Ok(())
    }

    /// Store the fitted artifact identity before any passive validation runs.
    pub fn lock_artifact(
        &mut self,
        identity: LockedArtifactIdentity,
    ) -> Result<(), CoordinatorError> {
        self.require_phase(CalibrationPhase::FitLockedArtifact, "lock artifact")?;
        self.artifact_identity = Some(identity);
        self.phase = CalibrationPhase::SurfaceValidate;
        Ok(())
    }

    /// Apply the locked passive-surface readiness result.
    pub fn submit_surface_validation(
        &mut self,
        validation: &ValidationReport,
    ) -> Result<(), CoordinatorError> {
        self.require_phase(
            CalibrationPhase::SurfaceValidate,
            "submit surface validation",
        )?;
        let identity =
            self.artifact_identity
                .as_ref()
                .ok_or(CoordinatorError::InvalidArtifactIdentity(
                    ArtifactIdentityError::MissingSourceSessions,
                ))?;
        let validation_block =
            self.surface_validation_block
                .ok_or(CoordinatorError::InvalidValidationEvidence(
                    ValidationEvidenceError::MissingValidationBlock,
                ))?;
        let training_blocks = self
            .training_blocks
            .iter()
            .copied()
            .map(BlockId::value)
            .collect::<Vec<_>>();
        let metrics = validation
            .validate_against(identity, validation_block, &training_blocks)
            .map_err(CoordinatorError::InvalidValidationEvidence)?;
        if validation.passes_held_out_validation(metrics) {
            self.phase = CalibrationPhase::ValidationPassed;
        } else {
            self.stop(
                CalibrationPhase::CalibrationFailed,
                CalibrationFailure::SurfaceValidation,
            );
        }
        Ok(())
    }

    /// Observe absolute setup time and enforce the finite 20-minute cap.
    pub fn observe_elapsed_seconds(
        &mut self,
        elapsed_seconds: u32,
    ) -> Result<(), CoordinatorError> {
        if elapsed_seconds < self.elapsed_seconds {
            return Err(CoordinatorError::ElapsedTimeRegression {
                previous_seconds: self.elapsed_seconds,
                actual_seconds: elapsed_seconds,
            });
        }
        self.elapsed_seconds = elapsed_seconds;
        if elapsed_seconds >= SETUP_TIME_CAP_SECONDS && !self.phase.is_terminal() {
            self.stop(
                CalibrationPhase::CalibrationFailed,
                CalibrationFailure::TimeCap,
            );
        }
        Ok(())
    }

    /// Stop collection on a topology, timing, or mount-health fault.
    pub fn report_hardware_fault(&mut self) {
        if self.phase == CalibrationPhase::ValidationPassed || !self.phase.is_terminal() {
            self.stop(
                CalibrationPhase::NeedsHardwareAttention,
                CalibrationFailure::HardwareFault,
            );
        }
    }

    /// Abort setup without creating a validated artifact or replacing a terminal failure.
    pub fn abort(&mut self) {
        if self.failure.is_none() && self.phase != CalibrationPhase::Aborted {
            self.phase = CalibrationPhase::Aborted;
        }
    }

    fn require_phase(
        &self,
        expected: CalibrationPhase,
        action: &'static str,
    ) -> Result<(), CoordinatorError> {
        if self.phase == expected {
            return Ok(());
        }
        Err(CoordinatorError::InvalidTransition {
            phase: self.phase,
            action,
        })
    }

    fn register_training_block(&mut self, block: BlockId) -> Result<(), CoordinatorError> {
        if self.validation_blocks.contains(&block) {
            return Err(CoordinatorError::BlockOverlap(block));
        }
        self.training_blocks.insert(block);
        Ok(())
    }

    fn register_validation_block(&mut self, block: BlockId) -> Result<(), CoordinatorError> {
        if self.training_blocks.contains(&block) {
            return Err(CoordinatorError::BlockOverlap(block));
        }
        if self.validation_blocks.contains(&block) {
            return Err(CoordinatorError::DuplicateValidationBlock(block));
        }
        self.validation_blocks.insert(block);
        Ok(())
    }

    fn observe_keyboard_improvement(&mut self, precheck_score: u16) {
        if let Some(previous) = self.previous_keyboard_precheck_score {
            let improvement = precheck_score.saturating_sub(previous);
            if improvement < MINIMUM_IMPROVEMENT_BASIS_POINTS {
                self.plateau_blocks = self.plateau_blocks.saturating_add(1);
            } else {
                self.plateau_blocks = 0;
            }
        }
        self.previous_keyboard_precheck_score = Some(precheck_score);
    }

    fn stop(&mut self, phase: CalibrationPhase, failure: CalibrationFailure) {
        self.phase = phase;
        self.failure = Some(failure);
    }
}

fn next_position(
    coverage: &[PositionCoverage; PHYSICAL_POSITION_COUNT],
    deficit: impl Fn(PositionCoverage) -> u16,
) -> Option<PhysicalPosition> {
    let mut selected = None;
    let mut selected_deficit = 0;
    for (index, item) in coverage.iter().copied().enumerate() {
        let item_deficit = deficit(item);
        if item_deficit > selected_deficit {
            selected = Some(all_slots()[index].position);
            selected_deficit = item_deficit;
        }
    }
    selected
}

fn progress_totals(coverage: &[PositionCoverage; PHYSICAL_POSITION_COUNT]) -> (u32, u32) {
    coverage.iter().fold((0, 0), |(accepted, required), item| {
        (
            accepted + u32::from(item.accepted.min(item.required)),
            required + u32::from(item.required),
        )
    })
}

const fn can_raise_requirement(coverage: PositionCoverage) -> bool {
    if coverage.accepted >= MAX_EXAMPLES_PER_POSITION
        || coverage.required >= MAX_EXAMPLES_PER_POSITION
    {
        return false;
    }
    true
}
