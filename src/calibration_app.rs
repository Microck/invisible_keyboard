//! Local-only guided calibration state and a dependency-free loopback HTTP service.

use std::{
    collections::BTreeMap,
    error::Error,
    fmt,
    fs::File,
    io::{self, Read, Write},
    net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener, TcpStream},
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, Instant},
};

use serde::{Deserialize, Deserializer, Serialize};

use crate::{
    artifact::{
        CalibrationArtifact, KeyboardLayoutConfirmationReceipt, US_QWERTY_CONFIRMATION_COPY,
        US_QWERTY_CONFIRMATION_PROMPT_ID, US_QWERTY_LAYOUT_ID,
    },
    calibration::{
        BlockId, CalibrationCoordinator, CalibrationDomain, CalibrationFailure, CalibrationPhase,
        CoordinatorError, KeyboardLayout, KeyboardPrecheckMetrics, LockedArtifactIdentity,
        RejectedExampleReason,
    },
    calibration_capture::CalibrationCaptureJournal,
    calibration_engine::{
        CalibrationEngineError, CalibrationEvidence, CalibrationFitInput, EvidenceDomain,
        EvidenceRole, KeyboardPrecheckResult, evaluate_keyboard_precheck, fit_calibration,
        validate_calibration,
    },
    clock_epoch::ClockEpochSet,
    labels::{HostTimeOrigin, MonotonicHostClock},
    layout::{
        Finger, Hand, PHYSICAL_POSITION_COUNT, PhysicalPosition, all_slots,
        core28_layout_manifest_bytes, keyboard_warm_start_eligible, position_index,
        slot_for_position,
    },
    manifest::{AcquisitionManifest, format_lower_hex, format_sha256, sha256},
    neutral::NeutralEvidenceReport,
    prompts::PromptCorpus,
    session::FinalizedSession,
    validation::{
        PredictionClass, TruthClass, ValidationMatrix, ValidationReport, ValidationReportInput,
    },
};

const FRONTEND_HTML: &str = include_str!("../web/calibration.html");
const FRONTEND_JS: &str = include_str!("../web/calibration-app.js");
const FRONTEND_CSS: &str = include_str!("../web/calibration.css");
const MAX_HTTP_REQUEST_BYTES: usize = 20_480;
const MAX_HTTP_BODY_BYTES: usize = 4_096;
// Each request read and response write has a cumulative 100 ms budget, not
// a fresh timeout per chunk. A progressing peer cannot hold the capture loop.
pub(crate) const HTTP_CONNECTION_TIMEOUT: Duration = Duration::from_millis(100);
const CAPTURE_LEASE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EvidenceMode {
    Hardware,
    RecordedReplay,
    SimulatedReplay,
}

impl EvidenceMode {
    const fn label(self) -> &'static str {
        match self {
            Self::Hardware => "hardware",
            Self::RecordedReplay => "recorded-replay",
            Self::SimulatedReplay => "simulated-replay",
        }
    }

    const fn limitation(self) -> &'static str {
        match self {
            Self::Hardware => {
                "Hardware evidence is accepted only after stream and validation gates."
            }
            Self::RecordedReplay => {
                "Immutable files drove the real fitter and state machine; physical meaning depends on capture provenance."
            }
            Self::SimulatedReplay => {
                "This exercises the software flow and does not establish physical feasibility."
            }
        }
    }
}

/// Browser action accepted by the loopback calibration service.
///
/// Known actions use one fixed kebab-case wire vocabulary. The unknown variant
/// retains only an unrecognized wire value so the typed boundary can preserve
/// the service's existing diagnostic instead of turning it into a JSON-shape
/// error.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CalibrationAction {
    AcknowledgeResult,
    SurfaceInstructionRendered,
    ConfirmUsQwerty,
    RejectUsQwerty,
    AdvanceSimulatedReplay,
    Pause,
    Resume,
    Abort,
    Heartbeat,
    VisibilityHidden,
    Blur,
    Pagehide,
    Beforeunload,
    Offline,
    Unknown(String),
}

impl<'de> Deserialize<'de> for CalibrationAction {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let action = String::deserialize(deserializer)?;
        Ok(match action.as_str() {
            "acknowledge-result" => Self::AcknowledgeResult,
            "surface-instruction-rendered" => Self::SurfaceInstructionRendered,
            "confirm-us-qwerty" => Self::ConfirmUsQwerty,
            "reject-us-qwerty" => Self::RejectUsQwerty,
            "advance-simulated-replay" => Self::AdvanceSimulatedReplay,
            "pause" => Self::Pause,
            "resume" => Self::Resume,
            "abort" => Self::Abort,
            "heartbeat" => Self::Heartbeat,
            "visibility-hidden" => Self::VisibilityHidden,
            "blur" => Self::Blur,
            "pagehide" => Self::Pagehide,
            "beforeunload" => Self::Beforeunload,
            "offline" => Self::Offline,
            _ => Self::Unknown(action),
        })
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct CalibrationSnapshot {
    pub revision: u64,
    pub capture_lease_revision: Option<u64>,
    pub surface_instruction_revision: Option<u64>,
    pub evidence: String,
    pub limitation: String,
    pub phase: String,
    pub screen: String,
    pub hardware_health: String,
    pub keyboard_device: String,
    pub supported_keyboard_layout: String,
    pub keyboard_layout_prompt_id: String,
    pub keyboard_layout_instruction_revision: Option<u64>,
    pub keyboard_layout_confirmation: String,
    pub keyboard_layout_confirmation_revision: Option<u64>,
    pub keyboard_layout_confirmation_host_time_ns: Option<u64>,
    pub prompt: String,
    pub surface_target: String,
    pub surface_position: Option<String>,
    pub neutral_seconds: u8,
    pub elapsed_seconds: u32,
    pub keyboard_accepted: u32,
    pub keyboard_required: u32,
    pub keyboard_coverage_basis_points: Option<u16>,
    pub keyboard_intent_separation_basis_points: Option<u16>,
    pub keyboard_finger_agreement_basis_points: Option<u16>,
    pub keyboard_precheck_evaluated_windows: u32,
    pub keyboard_weak_positions: Vec<String>,
    pub keyboard_precheck: String,
    pub keyboard_accuracy: String,
    pub surface_accepted: u32,
    pub surface_required: u32,
    pub rejected_examples: u32,
    pub setup_progress_basis_points: u16,
    pub validation: String,
    pub heldout_validation_passed: bool,
    pub failure_cause: Option<String>,
    pub result_acknowledgement: String,
    pub result_acknowledgement_revision: Option<u64>,
    pub result_acknowledgement_host_time_ns: Option<u64>,
    pub paused: bool,
    pub last_error: Option<String>,
}

/// Receipt that the browser-side presentation guard completed and the local
/// service accepted one surface-cue acknowledgement.
///
/// `acknowledged_host_time_ns` uses the same process-relative monotonic clock
/// as sensor and keyboard capture. It is sampled when the HTTP connection is
/// accepted, before request parsing or a later capture-runner poll. It does not
/// prove that pixels appeared or that a person made contact.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SurfaceInstructionReceipt {
    pub instruction_revision: u64,
    pub acknowledged_host_time_ns: u64,
}

/// Canonical proof that the browser acknowledged the saved terminal result.
///
/// This receipt proves only that the acknowledgement request reached the
/// loopback service. It is not sensor evidence and does not prove that the
/// wearables remained on or in the validated position.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResultAcknowledgementReceipt {
    pub instruction_revision: u64,
    pub acknowledged_host_time_ns: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResultAcknowledgementState {
    Unavailable,
    Pending(u64),
    Acknowledged(ResultAcknowledgementReceipt),
}

impl ResultAcknowledgementState {
    const fn instruction_revision(self) -> Option<u64> {
        match self {
            Self::Pending(revision) => Some(revision),
            Self::Acknowledged(receipt) => Some(receipt.instruction_revision),
            Self::Unavailable => None,
        }
    }
}

struct CalibrationAppState {
    coordinator: CalibrationCoordinator,
    mode: EvidenceMode,
    revision: u64,
    paused: bool,
    capture_lease: Option<CaptureLease>,
    keyboard_precheck: Option<KeyboardPrecheckFeedback>,
    last_error: Option<String>,
    hardware_health: String,
    keyboard_device: String,
    keyboard_layout_decision: KeyboardLayoutDecisionState,
    keyboard_prompt: Option<String>,
    surface_target: Option<String>,
    surface_position: Option<PhysicalPosition>,
    surface_instruction: SurfaceInstructionState,
    result_acknowledgement: ResultAcknowledgementState,
    neutral_seconds: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SurfaceInstructionState {
    None,
    Pending(u64),
    Accepted(SurfaceInstructionReceipt),
    Consumed(SurfaceInstructionReceipt),
}

impl SurfaceInstructionState {
    const fn pending_revision(self) -> Option<u64> {
        match self {
            Self::Pending(revision) => Some(revision),
            Self::Accepted(receipt) => Some(receipt.instruction_revision),
            Self::None | Self::Consumed(_) => None,
        }
    }
}

struct CaptureLease {
    revision: u64,
    deadline: Instant,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum KeyboardLayoutDecisionState {
    WaitingForDevice,
    Pending {
        instruction_revision: u64,
    },
    Confirmed(KeyboardLayoutConfirmationReceipt),
    Rejected {
        instruction_revision: u64,
        acknowledged_host_time_ns: u64,
    },
}

/// Typed runner-facing control state. UI strings remain presentation only.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CalibrationControlState {
    pub phase: CalibrationPhase,
    pub failure: Option<CalibrationFailure>,
    pub capture_lease_revision: Option<u64>,
    pub paused: bool,
}

impl KeyboardLayoutDecisionState {
    const fn instruction_revision(&self) -> Option<u64> {
        match self {
            Self::Pending {
                instruction_revision,
            } => Some(*instruction_revision),
            Self::WaitingForDevice | Self::Confirmed(_) | Self::Rejected { .. } => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KeyboardLayoutDecision {
    ConfirmUsQwerty,
    Unsupported,
}

#[derive(Clone)]
struct KeyboardPrecheckFeedback {
    metrics: KeyboardPrecheckMetrics,
    evaluated_windows: u32,
    weak_positions: Vec<PhysicalPosition>,
}

/// Typed boundary between hardware capture and the local calibration UI.
///
/// The browser cannot fabricate these events. Only the in-process capture
/// worker can apply them after preserving the corresponding raw evidence.
pub enum HardwareCalibrationEvent {
    Connected,
    HardwareChecked,
    KeyboardDeviceIdentified {
        device: String,
    },
    NeutralAccepted,
    CaptureInstruction {
        keyboard_prompt: Option<String>,
        surface_target: Option<String>,
        surface_position: Option<PhysicalPosition>,
        surface_instruction_receipt_required: bool,
        neutral_seconds: Option<u8>,
        capture_lease_revision: u64,
    },
    ExampleAccepted {
        domain: CalibrationDomain,
        position: PhysicalPosition,
        block: BlockId,
        capture_lease_revision: u64,
    },
    ExampleRejected {
        reason: RejectedExampleReason,
        capture_lease_revision: u64,
    },
    RejectedExamplesRecorded(u32),
    BeginKeyboardPrecheck(BlockId),
    KeyboardPrecheck {
        metrics: KeyboardPrecheckMetrics,
        evaluated_windows: usize,
        weak_positions: Vec<PhysicalPosition>,
    },
    BeginArtifactFit(BlockId),
    ArtifactLocked(LockedArtifactIdentity),
    SurfaceValidation(ValidationReport),
    ElapsedSeconds(u32),
    HardwareFault(String),
}

#[derive(Clone)]
pub struct CalibrationApp {
    state: Arc<Mutex<CalibrationAppState>>,
}

impl CalibrationApp {
    #[must_use]
    pub fn new(mode: EvidenceMode) -> Self {
        Self {
            state: Arc::new(Mutex::new(CalibrationAppState {
                coordinator: CalibrationCoordinator::new(),
                mode,
                revision: 0,
                paused: false,
                capture_lease: None,
                keyboard_precheck: None,
                last_error: None,
                hardware_health: "not checked".to_owned(),
                keyboard_device: "Waiting for selected EV_KEY device".to_owned(),
                keyboard_layout_decision: KeyboardLayoutDecisionState::WaitingForDevice,
                keyboard_prompt: None,
                surface_target: None,
                surface_position: None,
                surface_instruction: SurfaceInstructionState::None,
                result_acknowledgement: ResultAcknowledgementState::Unavailable,
                neutral_seconds: 3,
            })),
        }
    }

    pub fn snapshot(&self) -> Result<CalibrationSnapshot, CalibrationAppError> {
        let mut state = self.lock()?;
        expire_overdue_capture_lease(&mut state);
        Ok(snapshot_from_state(&state))
    }

    /// Read terminal intent and lease ownership without parsing UI copy.
    pub fn control_state(&self) -> Result<CalibrationControlState, CalibrationAppError> {
        let mut state = self.lock()?;
        expire_overdue_capture_lease(&mut state);
        Ok(CalibrationControlState {
            phase: state.coordinator.phase(),
            failure: state.coordinator.failure(),
            capture_lease_revision: state.capture_lease.as_ref().map(|lease| lease.revision),
            paused: state.paused,
        })
    }

    /// Check one exact browser lease without constructing a UI snapshot.
    pub fn capture_lease_is_live(
        &self,
        expected_revision: u64,
    ) -> Result<bool, CalibrationAppError> {
        let mut state = self.lock()?;
        expire_overdue_capture_lease(&mut state);
        Ok(!state.paused
            && state.capture_lease.as_ref().is_some_and(|lease| {
                lease.revision == expected_revision && Instant::now() <= lease.deadline
            }))
    }

    /// Publish runner time without allocating a complete UI snapshot.
    pub fn observe_elapsed_seconds(&self, seconds: u32) -> Result<(), CalibrationAppError> {
        let mut state = self.lock()?;
        require_result_mutable(&state)?;
        if state.coordinator.progress().elapsed_seconds == seconds {
            return Ok(());
        }
        if let Err(error) = state.coordinator.observe_elapsed_seconds(seconds) {
            record_snapshot_error(&mut state, error.to_string());
            return Err(error.into());
        }
        state.revision = state.revision.saturating_add(1);
        state.last_error = None;
        Ok(())
    }

    /// Advance one deterministic simulated evidence block through the same
    /// coordinator used by the hardware service.
    pub fn advance_simulated_replay(&self) -> Result<(), CalibrationAppError> {
        let mut state = self.lock()?;
        require_result_mutable(&state)?;
        if state.mode != EvidenceMode::SimulatedReplay {
            return Err(CalibrationAppError::ActionNotAllowed);
        }
        if state.paused {
            return Err(CalibrationAppError::Paused);
        }
        advance_fixture(&mut state.coordinator)?;
        state.revision = state.revision.saturating_add(1);
        state.last_error = None;
        Ok(())
    }

    pub fn apply_action(
        &self,
        action: CalibrationAction,
        supplied_revision: u64,
        accepted_host_time_ns: u64,
    ) -> Result<CalibrationSnapshot, CalibrationAppError> {
        let mut state = self.lock()?;
        if action != CalibrationAction::AcknowledgeResult {
            require_result_mutable(&state)?;
        }
        match action {
            CalibrationAction::AcknowledgeResult => {
                acknowledge_result_locked(&mut state, supplied_revision, accepted_host_time_ns)?;
            }
            CalibrationAction::SurfaceInstructionRendered => {
                acknowledge_surface_instruction_locked(
                    &mut state,
                    supplied_revision,
                    accepted_host_time_ns,
                )?;
            }
            CalibrationAction::ConfirmUsQwerty => {
                decide_keyboard_layout_locked(
                    &mut state,
                    KeyboardLayoutDecision::ConfirmUsQwerty,
                    supplied_revision,
                    accepted_host_time_ns,
                )?;
            }
            CalibrationAction::RejectUsQwerty => {
                decide_keyboard_layout_locked(
                    &mut state,
                    KeyboardLayoutDecision::Unsupported,
                    supplied_revision,
                    accepted_host_time_ns,
                )?;
            }
            CalibrationAction::AdvanceSimulatedReplay => {
                require_state_revision(&state, supplied_revision)?;
                if state.mode != EvidenceMode::SimulatedReplay {
                    return Err(CalibrationAppError::ActionNotAllowed);
                }
                if state.paused {
                    return Err(CalibrationAppError::Paused);
                }
                advance_fixture(&mut state.coordinator)?;
                state.revision = state.revision.saturating_add(1);
                state.last_error = None;
            }
            CalibrationAction::Pause => {
                require_state_revision(&state, supplied_revision)?;
                state.paused = true;
                state.capture_lease = None;
                clear_surface_instruction(&mut state);
                state.revision = state.revision.saturating_add(1);
                state.last_error = None;
            }
            CalibrationAction::Resume => {
                require_state_revision(&state, supplied_revision)?;
                if state.mode == EvidenceMode::Hardware {
                    return Err(CalibrationAppError::ActionNotAllowed);
                }
                if state.capture_lease.is_some() {
                    return Err(CalibrationAppError::CaptureLeaseAlreadyActive);
                }
                state.paused = false;
                state.revision = state.revision.saturating_add(1);
                state.capture_lease = Some(CaptureLease {
                    revision: state.revision,
                    deadline: Instant::now() + CAPTURE_LEASE_TIMEOUT,
                });
                state.last_error = None;
            }
            CalibrationAction::Abort => {
                // Stop intent applies to the attempt, not a rendered progress snapshot.
                state.coordinator.abort();
                state.paused = true;
                state.capture_lease = None;
                clear_surface_instruction(&mut state);
                state.revision = state.revision.saturating_add(1);
                if state.coordinator.failure().is_none() {
                    state.last_error = None;
                }
            }
            CalibrationAction::Heartbeat => {
                refresh_capture_lease(&mut state, supplied_revision)?;
            }
            CalibrationAction::VisibilityHidden
            | CalibrationAction::Blur
            | CalibrationAction::Pagehide
            | CalibrationAction::Beforeunload
            | CalibrationAction::Offline => {
                let message = lifecycle_capture_loss_message(state.mode);
                expire_capture_lease_locked(&mut state, supplied_revision, message)?;
            }
            CalibrationAction::Unknown(action) => {
                return Err(CalibrationAppError::UnknownAction(action));
            }
        }
        Ok(snapshot_from_state(&state))
    }

    /// Record the exact layout instruction shown by the browser.
    ///
    /// The instruction revision is independent from the global state revision,
    /// so elapsed-time updates cannot make a visible confirmation stale.
    pub fn decide_keyboard_layout(
        &self,
        decision: KeyboardLayoutDecision,
        instruction_revision: u64,
        acknowledged_host_time_ns: u64,
    ) -> Result<CalibrationSnapshot, CalibrationAppError> {
        let mut state = self.lock()?;
        require_result_mutable(&state)?;
        decide_keyboard_layout_locked(
            &mut state,
            decision,
            instruction_revision,
            acknowledged_host_time_ns,
        )?;
        Ok(snapshot_from_state(&state))
    }

    /// Arm one hardware capture only after the runner persists and binds the
    /// positive layout receipt and all keyboard keys have been released.
    pub fn activate_confirmed_layout_capture(
        &self,
        instruction_revision: u64,
    ) -> Result<CalibrationSnapshot, CalibrationAppError> {
        let mut state = self.lock()?;
        require_result_mutable(&state)?;
        let confirmed = matches!(
            &state.keyboard_layout_decision,
            KeyboardLayoutDecisionState::Confirmed(receipt)
                if receipt.instruction_revision == instruction_revision
        );
        if state.mode != EvidenceMode::Hardware
            || !confirmed
            || state.coordinator.phase() != CalibrationPhase::NeutralPose
            || state.paused
            || state.capture_lease.is_some()
        {
            return Err(CalibrationAppError::ActionNotAllowed);
        }
        state.revision = state.revision.saturating_add(1);
        state.capture_lease = Some(CaptureLease {
            revision: state.revision,
            deadline: Instant::now() + CAPTURE_LEASE_TIMEOUT,
        });
        state.last_error = None;
        Ok(snapshot_from_state(&state))
    }

    /// Return the immutable positive receipt for runner persistence/binding.
    pub fn keyboard_layout_confirmation_receipt(
        &self,
    ) -> Result<Option<KeyboardLayoutConfirmationReceipt>, CalibrationAppError> {
        let state = self.lock()?;
        Ok(match &state.keyboard_layout_decision {
            KeyboardLayoutDecisionState::Confirmed(receipt) => Some(receipt.clone()),
            KeyboardLayoutDecisionState::WaitingForDevice
            | KeyboardLayoutDecisionState::Pending { .. }
            | KeyboardLayoutDecisionState::Rejected { .. } => None,
        })
    }

    pub fn keyboard_layout_decision_state(
        &self,
    ) -> Result<KeyboardLayoutDecisionState, CalibrationAppError> {
        Ok(self.lock()?.keyboard_layout_decision.clone())
    }

    /// Reproduce a receipt already bound into immutable recorded evidence.
    pub fn apply_recorded_keyboard_layout_confirmation(
        &self,
        receipt: KeyboardLayoutConfirmationReceipt,
    ) -> Result<CalibrationSnapshot, CalibrationAppError> {
        receipt
            .validate()
            .map_err(|_| CalibrationAppError::InvalidInstruction)?;
        let mut state = self.lock()?;
        require_result_mutable(&state)?;
        if state.mode != EvidenceMode::RecordedReplay
            || state.coordinator.phase() != CalibrationPhase::KeyboardIdentify
            || !matches!(
                &state.keyboard_layout_decision,
                KeyboardLayoutDecisionState::Pending { .. }
            )
        {
            return Err(CalibrationAppError::ActionNotAllowed);
        }
        state
            .coordinator
            .identify_keyboard(KeyboardLayout::UsQwerty)?;
        state.keyboard_layout_decision = KeyboardLayoutDecisionState::Confirmed(receipt);
        state.revision = state.revision.saturating_add(1);
        state.capture_lease = Some(CaptureLease {
            revision: state.revision,
            deadline: Instant::now() + CAPTURE_LEASE_TIMEOUT,
        });
        Ok(snapshot_from_state(&state))
    }

    /// Accept one browser render acknowledgement at its HTTP receipt time.
    ///
    /// The first accepted time for the current instruction is canonical.
    /// Duplicate requests, including a retry after runner consumption, are
    /// successful no-ops while that instruction remains the current one.
    pub fn acknowledge_surface_instruction(
        &self,
        instruction_revision: u64,
        acknowledged_host_time_ns: u64,
    ) -> Result<CalibrationSnapshot, CalibrationAppError> {
        let mut state = self.lock()?;
        require_result_mutable(&state)?;
        acknowledge_surface_instruction_locked(
            &mut state,
            instruction_revision,
            acknowledged_host_time_ns,
        )?;
        Ok(snapshot_from_state(&state))
    }

    /// Consume one browser receipt after the cue text reached the rendered DOM.
    ///
    /// The receipt proves presentation timing only. The immutable plan remains
    /// the source of target identity, and selected hardware streams remain the
    /// source of sensor evidence.
    pub fn take_surface_instruction_ack(
        &self,
        instruction_revision: u64,
    ) -> Result<Option<SurfaceInstructionReceipt>, CalibrationAppError> {
        let mut state = self.lock()?;
        let SurfaceInstructionState::Accepted(receipt) = state.surface_instruction else {
            return Ok(None);
        };
        if receipt.instruction_revision != instruction_revision {
            return Ok(None);
        }
        state.surface_instruction = SurfaceInstructionState::Consumed(receipt);
        state.revision = state.revision.saturating_add(1);
        Ok(Some(receipt))
    }

    /// Expire a capture when the service watchdog stops receiving heartbeats.
    ///
    /// The supplied lease identity prevents a delayed watchdog from revoking a
    /// newer browser lease. The browser cannot extend the lease without the
    /// local service accepting a matching heartbeat.
    pub fn expire_capture_lease(
        &self,
        supplied_revision: u64,
    ) -> Result<CalibrationSnapshot, CalibrationAppError> {
        let mut state = self.lock()?;
        let message = heartbeat_capture_loss_message(state.mode);
        expire_capture_lease_locked(&mut state, supplied_revision, message)?;
        Ok(snapshot_from_state(&state))
    }

    /// Apply one evidence-backed event from the hardware capture worker.
    pub fn apply_hardware_event(
        &self,
        event: HardwareCalibrationEvent,
    ) -> Result<CalibrationSnapshot, CalibrationAppError> {
        let mut state = self.lock()?;
        require_result_mutable(&state)?;
        if state.mode == EvidenceMode::SimulatedReplay {
            return Err(CalibrationAppError::ActionNotAllowed);
        }
        if state.paused
            && !matches!(
                &event,
                HardwareCalibrationEvent::HardwareFault(_)
                    | HardwareCalibrationEvent::ElapsedSeconds(_)
                    | HardwareCalibrationEvent::ExampleAccepted { .. }
                    | HardwareCalibrationEvent::ExampleRejected { .. }
            )
        {
            return Err(CalibrationAppError::Paused);
        }
        // The coordinator is the sole elapsed-time store. Treat an exact
        // repeat as a no-op so the runner can publish its derived clock value
        // every loop without a second mutable "last second" field.
        if let HardwareCalibrationEvent::ElapsedSeconds(seconds) = &event
            && state.coordinator.progress().elapsed_seconds == *seconds
        {
            return Ok(snapshot_from_state(&state));
        }
        let result = match event {
            HardwareCalibrationEvent::Connected => state.coordinator.connect(),
            HardwareCalibrationEvent::HardwareChecked => {
                let result = state.coordinator.accept_hardware_check();
                if result.is_ok() {
                    state.hardware_health = "passed recorded checks".to_owned();
                }
                result
            }
            HardwareCalibrationEvent::KeyboardDeviceIdentified { device } => {
                if state.coordinator.phase() != CalibrationPhase::KeyboardIdentify
                    || !matches!(
                        &state.keyboard_layout_decision,
                        KeyboardLayoutDecisionState::WaitingForDevice
                    )
                    || device.trim().is_empty()
                {
                    return Err(CalibrationAppError::ActionNotAllowed);
                }
                state.keyboard_device = device;
                state.keyboard_layout_decision = KeyboardLayoutDecisionState::Pending {
                    instruction_revision: state.revision.saturating_add(1),
                };
                Ok(())
            }
            HardwareCalibrationEvent::NeutralAccepted => {
                let lease_revision = state
                    .capture_lease
                    .as_ref()
                    .map(|lease| lease.revision)
                    .ok_or(CalibrationAppError::CaptureLeaseRequired)?;
                require_capture_lease(&mut state, lease_revision)?;
                state.coordinator.accept_neutral_pose()
            }
            HardwareCalibrationEvent::CaptureInstruction {
                keyboard_prompt,
                surface_target,
                surface_position,
                surface_instruction_receipt_required,
                neutral_seconds,
                capture_lease_revision,
            } => {
                require_capture_lease(&mut state, capture_lease_revision)?;
                if keyboard_prompt
                    .as_ref()
                    .is_some_and(|instruction| instruction.trim().is_empty())
                    || surface_target
                        .as_ref()
                        .is_some_and(|instruction| instruction.trim().is_empty())
                    || (surface_instruction_receipt_required && surface_target.is_none())
                    || (surface_position.is_some() && surface_target.is_none())
                    || surface_position.is_some_and(|position| position_index(position).is_none())
                    || neutral_seconds.is_some_and(|seconds| seconds > 3)
                {
                    return Err(CalibrationAppError::InvalidInstruction);
                }
                if let Some(instruction) = keyboard_prompt {
                    state.keyboard_prompt = Some(instruction);
                }
                if let Some(target) = surface_target {
                    state.surface_target = Some(target);
                    state.surface_position = surface_position;
                    clear_surface_instruction(&mut state);
                    if surface_instruction_receipt_required {
                        state.surface_instruction =
                            SurfaceInstructionState::Pending(state.revision.saturating_add(1));
                    }
                }
                if let Some(seconds) = neutral_seconds {
                    state.neutral_seconds = seconds;
                }
                Ok(())
            }
            HardwareCalibrationEvent::ExampleAccepted {
                domain,
                position,
                block,
                capture_lease_revision,
            } => {
                require_capture_lease(&mut state, capture_lease_revision)?;
                state.coordinator.accept_example(domain, position, block)
            }
            HardwareCalibrationEvent::ExampleRejected {
                reason,
                capture_lease_revision,
            } => {
                require_capture_lease(&mut state, capture_lease_revision)?;
                state.coordinator.reject_example(reason)
            }
            HardwareCalibrationEvent::RejectedExamplesRecorded(count) => {
                state.coordinator.reject_examples(count)
            }
            HardwareCalibrationEvent::BeginKeyboardPrecheck(block) => {
                state.coordinator.begin_keyboard_precheck(block)
            }
            HardwareCalibrationEvent::KeyboardPrecheck {
                metrics,
                evaluated_windows,
                weak_positions,
            } => {
                let result = state
                    .coordinator
                    .submit_keyboard_precheck(metrics, &weak_positions);
                if result.is_ok() {
                    state.keyboard_precheck = Some(KeyboardPrecheckFeedback {
                        metrics,
                        evaluated_windows: u32::try_from(evaluated_windows).unwrap_or(u32::MAX),
                        weak_positions,
                    });
                }
                result
            }
            HardwareCalibrationEvent::BeginArtifactFit(block) => {
                state.coordinator.begin_artifact_fit(block)
            }
            HardwareCalibrationEvent::ArtifactLocked(identity) => {
                state.coordinator.lock_artifact(identity)
            }
            HardwareCalibrationEvent::SurfaceValidation(report) => {
                let result = state.coordinator.submit_surface_validation(&report);
                if result.is_ok() {
                    state.capture_lease = None;
                    clear_surface_instruction(&mut state);
                }
                result
            }
            HardwareCalibrationEvent::ElapsedSeconds(seconds) => {
                state.coordinator.observe_elapsed_seconds(seconds)
            }
            HardwareCalibrationEvent::HardwareFault(message) => {
                state.hardware_health = format!("failed: {message}");
                state.paused = true;
                state.capture_lease = None;
                clear_surface_instruction(&mut state);
                state.coordinator.report_hardware_fault();
                Ok(())
            }
        };
        if let Err(error) = result {
            record_snapshot_error(&mut state, error.to_string());
            return Err(error.into());
        }
        state.revision = state.revision.saturating_add(1);
        state.last_error = None;
        Ok(snapshot_from_state(&state))
    }

    /// Publish the already-persisted validation result for browser review.
    ///
    /// The caller must durably save the model and evidence first. Publication
    /// starts a dedicated acknowledgement revision and makes every other
    /// browser mutation read-only.
    pub fn publish_saved_result(&self) -> Result<CalibrationSnapshot, CalibrationAppError> {
        let mut state = self.lock()?;
        if state.coordinator.phase() != CalibrationPhase::ValidationPassed {
            return Err(CalibrationAppError::ActionNotAllowed);
        }
        if state.result_acknowledgement == ResultAcknowledgementState::Unavailable {
            state.revision = state.revision.saturating_add(1);
            state.result_acknowledgement = ResultAcknowledgementState::Pending(state.revision);
            state.capture_lease = None;
            clear_surface_instruction(&mut state);
            state.last_error = None;
        }
        Ok(snapshot_from_state(&state))
    }

    /// Record explicit browser acknowledgement of the saved result.
    ///
    /// Like surface-cue acknowledgement, this is an intent and timing receipt
    /// only. It cannot assert wearable contact, placement, or continuity.
    pub fn acknowledge_result(
        &self,
        instruction_revision: u64,
        acknowledged_host_time_ns: u64,
    ) -> Result<CalibrationSnapshot, CalibrationAppError> {
        let mut state = self.lock()?;
        acknowledge_result_locked(&mut state, instruction_revision, acknowledged_host_time_ns)?;
        Ok(snapshot_from_state(&state))
    }

    pub fn result_acknowledgement_state(
        &self,
    ) -> Result<ResultAcknowledgementState, CalibrationAppError> {
        Ok(self.lock()?.result_acknowledgement)
    }

    pub fn result_acknowledgement_receipt(
        &self,
    ) -> Result<Option<ResultAcknowledgementReceipt>, CalibrationAppError> {
        Ok(match self.lock()?.result_acknowledgement {
            ResultAcknowledgementState::Acknowledged(receipt) => Some(receipt),
            ResultAcknowledgementState::Unavailable | ResultAcknowledgementState::Pending(_) => {
                None
            }
        })
    }

    fn lock(&self) -> Result<MutexGuard<'_, CalibrationAppState>, CalibrationAppError> {
        self.state
            .lock()
            .map_err(|_| CalibrationAppError::StatePoisoned)
    }
}

fn decide_keyboard_layout_locked(
    state: &mut CalibrationAppState,
    decision: KeyboardLayoutDecision,
    instruction_revision: u64,
    acknowledged_host_time_ns: u64,
) -> Result<(), CalibrationAppError> {
    if state.mode != EvidenceMode::Hardware {
        return Err(CalibrationAppError::ActionNotAllowed);
    }
    match &state.keyboard_layout_decision {
        KeyboardLayoutDecisionState::Confirmed(receipt)
            if decision == KeyboardLayoutDecision::ConfirmUsQwerty
                && receipt.instruction_revision == instruction_revision =>
        {
            return Ok(());
        }
        KeyboardLayoutDecisionState::Rejected {
            instruction_revision: existing,
            ..
        } if decision == KeyboardLayoutDecision::Unsupported
            && *existing == instruction_revision =>
        {
            return Ok(());
        }
        KeyboardLayoutDecisionState::Confirmed(_)
        | KeyboardLayoutDecisionState::Rejected { .. } => {
            return Err(CalibrationAppError::KeyboardLayoutDecisionConflict);
        }
        KeyboardLayoutDecisionState::Pending {
            instruction_revision: pending,
        } if *pending == instruction_revision => {}
        KeyboardLayoutDecisionState::WaitingForDevice
        | KeyboardLayoutDecisionState::Pending { .. } => {
            return Err(CalibrationAppError::KeyboardLayoutInstructionNotPending);
        }
    }

    match decision {
        KeyboardLayoutDecision::ConfirmUsQwerty => {
            state
                .coordinator
                .identify_keyboard(KeyboardLayout::UsQwerty)?;
            state.keyboard_layout_decision = KeyboardLayoutDecisionState::Confirmed(
                KeyboardLayoutConfirmationReceipt::us_qwerty(
                    instruction_revision,
                    acknowledged_host_time_ns,
                ),
            );
            state.last_error = None;
        }
        KeyboardLayoutDecision::Unsupported => {
            state
                .coordinator
                .identify_keyboard(KeyboardLayout::Unsupported)?;
            state.keyboard_layout_decision = KeyboardLayoutDecisionState::Rejected {
                instruction_revision,
                acknowledged_host_time_ns,
            };
            state.paused = true;
            state.capture_lease = None;
            state.last_error =
                Some("This setup supports US QWERTY physical key positions only.".to_owned());
        }
    }
    state.revision = state.revision.saturating_add(1);
    Ok(())
}

fn acknowledge_surface_instruction_locked(
    state: &mut CalibrationAppState,
    instruction_revision: u64,
    acknowledged_host_time_ns: u64,
) -> Result<(), CalibrationAppError> {
    if state.mode != EvidenceMode::Hardware {
        return Err(CalibrationAppError::SurfaceInstructionNotPending);
    }
    let lease_revision = state
        .capture_lease
        .as_ref()
        .map(|lease| lease.revision)
        .ok_or(CalibrationAppError::CaptureLeaseRequired)?;
    require_capture_lease(state, lease_revision)?;
    match state.surface_instruction {
        SurfaceInstructionState::Pending(revision) if revision == instruction_revision => {
            state.surface_instruction =
                SurfaceInstructionState::Accepted(SurfaceInstructionReceipt {
                    instruction_revision,
                    acknowledged_host_time_ns,
                });
        }
        SurfaceInstructionState::Accepted(receipt) | SurfaceInstructionState::Consumed(receipt)
            if receipt.instruction_revision == instruction_revision =>
        {
            // The first accepted receipt is canonical. Retrying the same
            // request cannot replace its timestamp or make it consumable a
            // second time.
        }
        SurfaceInstructionState::None
        | SurfaceInstructionState::Pending(_)
        | SurfaceInstructionState::Accepted(_)
        | SurfaceInstructionState::Consumed(_) => {
            return Err(CalibrationAppError::SurfaceInstructionNotPending);
        }
    }
    Ok(())
}

fn acknowledge_result_locked(
    state: &mut CalibrationAppState,
    instruction_revision: u64,
    acknowledged_host_time_ns: u64,
) -> Result<(), CalibrationAppError> {
    match state.result_acknowledgement {
        ResultAcknowledgementState::Pending(revision) if revision == instruction_revision => {
            state.result_acknowledgement =
                ResultAcknowledgementState::Acknowledged(ResultAcknowledgementReceipt {
                    instruction_revision,
                    acknowledged_host_time_ns,
                });
            state.revision = state.revision.saturating_add(1);
        }
        ResultAcknowledgementState::Acknowledged(receipt)
            if receipt.instruction_revision == instruction_revision =>
        {
            // Preserve the first accepted service time across retries.
        }
        ResultAcknowledgementState::Unavailable
        | ResultAcknowledgementState::Pending(_)
        | ResultAcknowledgementState::Acknowledged(_) => {
            return Err(CalibrationAppError::ResultAcknowledgementNotPending);
        }
    }
    Ok(())
}

fn require_result_mutable(state: &CalibrationAppState) -> Result<(), CalibrationAppError> {
    if state.result_acknowledgement == ResultAcknowledgementState::Unavailable {
        Ok(())
    } else {
        Err(CalibrationAppError::SavedResultReadOnly)
    }
}

fn record_snapshot_error(state: &mut CalibrationAppState, message: String) {
    if state.last_error.as_ref() == Some(&message) {
        return;
    }
    state.last_error = Some(message);
    state.revision = state.revision.saturating_add(1);
}

fn record_capture_loss(state: &mut CalibrationAppState, message: &str) {
    state.capture_lease = None;
    state.paused = true;
    clear_surface_instruction(state);
    state.last_error = Some(message.to_owned());
    state.revision = state.revision.saturating_add(1);
}

fn require_capture_lease(
    state: &mut CalibrationAppState,
    supplied_revision: u64,
) -> Result<(), CalibrationAppError> {
    let Some(lease) = state.capture_lease.as_mut() else {
        return Err(CalibrationAppError::CaptureLeaseRequired);
    };
    if lease.revision != supplied_revision {
        return Err(CalibrationAppError::StaleCaptureLease);
    }
    if Instant::now() > lease.deadline {
        record_capture_loss(state, heartbeat_capture_loss_message(state.mode));
        return Err(CalibrationAppError::StaleCaptureLease);
    }
    Ok(())
}

fn require_state_revision(
    state: &CalibrationAppState,
    supplied_revision: u64,
) -> Result<(), CalibrationAppError> {
    if state.revision != supplied_revision {
        return Err(CalibrationAppError::StaleStateRevision {
            expected: state.revision,
            supplied: supplied_revision,
        });
    }
    Ok(())
}

fn refresh_capture_lease(
    state: &mut CalibrationAppState,
    supplied_revision: u64,
) -> Result<(), CalibrationAppError> {
    let Some(lease) = state.capture_lease.as_mut() else {
        return Err(CalibrationAppError::CaptureLeaseRequired);
    };
    if lease.revision != supplied_revision {
        return Err(CalibrationAppError::StaleCaptureLease);
    }
    if Instant::now() > lease.deadline {
        record_capture_loss(state, heartbeat_capture_loss_message(state.mode));
        return Err(CalibrationAppError::StaleCaptureLease);
    }
    lease.deadline = Instant::now() + CAPTURE_LEASE_TIMEOUT;
    Ok(())
}

fn expire_capture_lease_locked(
    state: &mut CalibrationAppState,
    supplied_revision: u64,
    message: &str,
) -> Result<(), CalibrationAppError> {
    let Some(lease) = state.capture_lease.as_ref() else {
        return Err(CalibrationAppError::CaptureLeaseRequired);
    };
    if lease.revision != supplied_revision {
        return Err(CalibrationAppError::StaleCaptureLease);
    }
    record_capture_loss(state, message);
    Ok(())
}

fn expire_overdue_capture_lease(state: &mut CalibrationAppState) {
    let overdue = state
        .capture_lease
        .as_ref()
        .is_some_and(|lease| Instant::now() > lease.deadline);
    if overdue {
        record_capture_loss(state, heartbeat_capture_loss_message(state.mode));
    }
}

fn clear_surface_instruction(state: &mut CalibrationAppState) {
    state.surface_instruction = SurfaceInstructionState::None;
}

const fn lifecycle_capture_loss_message(mode: EvidenceMode) -> &'static str {
    match mode {
        EvidenceMode::Hardware => "browser capture stopped; restart the setup command",
        EvidenceMode::RecordedReplay | EvidenceMode::SimulatedReplay => {
            "browser capture stopped; press Resume to continue"
        }
    }
}

const fn heartbeat_capture_loss_message(mode: EvidenceMode) -> &'static str {
    match mode {
        EvidenceMode::Hardware => "capture heartbeat expired; restart the setup command",
        EvidenceMode::RecordedReplay | EvidenceMode::SimulatedReplay => {
            "capture heartbeat expired; press Resume to continue"
        }
    }
}

/// Immutable capture files required to replay one complete calibration attempt.
pub struct CaptureCalibrationInput<'a> {
    pub manifest: &'a AcquisitionManifest,
    pub fitting_clocks: &'a ClockEpochSet,
    pub fitting_journal: &'a CalibrationCaptureJournal,
    pub fitting_session: &'a FinalizedSession,
    pub neutral_report: &'a NeutralEvidenceReport,
    pub validation_clocks: &'a ClockEpochSet,
    pub validation_journal: &'a CalibrationCaptureJournal,
    pub validation_session: &'a FinalizedSession,
}

/// Artifacts, diagnostics, and final UI state produced by one capture-bound replay.
pub struct CaptureCalibrationRun {
    pub artifact: CalibrationArtifact,
    pub validation_report: ValidationReport,
    pub fitting_evidence: CalibrationEvidence,
    pub validation_evidence: CalibrationEvidence,
    pub keyboard_precheck: KeyboardPrecheckResult,
    pub snapshot: CalibrationSnapshot,
}

/// Drive the user-visible state machine from verified fitting and held-out data.
///
/// This function validates every immutable input before it advances the UI. It
/// makes the software flow executable without changing the evidence label of
/// a recorded or generated fixture into a hardware claim.
pub fn run_capture_calibration(
    app: &CalibrationApp,
    input: CaptureCalibrationInput<'_>,
) -> Result<CaptureCalibrationRun, CaptureCalibrationRunError> {
    let precheck = evaluate_keyboard_precheck(
        input.manifest,
        input.fitting_clocks,
        input.fitting_journal,
        input.fitting_session,
    )?;
    let fitted = fit_calibration(CalibrationFitInput {
        manifest: input.manifest,
        clocks: input.fitting_clocks,
        journal: input.fitting_journal,
        session: input.fitting_session,
        neutral_report: input.neutral_report,
    })?;
    let validated = validate_calibration(
        input.manifest,
        input.validation_clocks,
        &fitted.artifact,
        input.validation_journal,
        input.validation_session,
    )?;
    let fitting_evidence = &fitted.diagnostic_evidence;
    let validation_evidence = &validated.diagnostic_evidence;
    if validation_evidence.setup_seconds() < fitting_evidence.setup_seconds() {
        return Err(CaptureCalibrationRunError::ElapsedTimeRegression);
    }

    app.apply_hardware_event(HardwareCalibrationEvent::Connected)?;
    app.apply_hardware_event(HardwareCalibrationEvent::HardwareChecked)?;
    app.apply_hardware_event(HardwareCalibrationEvent::KeyboardDeviceIdentified {
        device: format!(
            "{} ({})",
            fitting_evidence.keyboard().name,
            fitting_evidence.keyboard().physical_path
        ),
    })?;
    let confirmed = app.apply_recorded_keyboard_layout_confirmation(
        fitting_evidence.keyboard().layout_confirmation.clone(),
    )?;
    let capture_lease_revision = confirmed
        .capture_lease_revision
        .ok_or(CalibrationAppError::CaptureLeaseRequired)?;
    app.apply_hardware_event(HardwareCalibrationEvent::NeutralAccepted)?;
    app.apply_hardware_event(HardwareCalibrationEvent::RejectedExamplesRecorded(
        fitting_evidence.rejected_examples(),
    ))?;
    app.apply_hardware_event(HardwareCalibrationEvent::ElapsedSeconds(
        fitting_evidence.setup_seconds(),
    ))?;

    for observation in fitting_evidence
        .observations()
        .iter()
        .filter(|observation| {
            observation.role() == EvidenceRole::Training
                && observation.domain() == EvidenceDomain::Keyboard
                && observation.position().is_some()
        })
    {
        app.apply_hardware_event(HardwareCalibrationEvent::ExampleAccepted {
            domain: CalibrationDomain::Keyboard,
            position: observation.position().expect("filter retained a position"),
            block: BlockId::new(observation.block_id()),
            capture_lease_revision,
        })?;
    }
    let threshold_block = fitting_evidence
        .observations()
        .iter()
        .filter(|observation| observation.role() == EvidenceRole::ThresholdSelection)
        .map(|observation| observation.block_id())
        .min()
        .ok_or(CaptureCalibrationRunError::MissingThresholdBlock)?;
    app.apply_hardware_event(HardwareCalibrationEvent::BeginKeyboardPrecheck(
        BlockId::new(threshold_block),
    ))?;
    app.apply_hardware_event(HardwareCalibrationEvent::KeyboardPrecheck {
        metrics: precheck.metrics,
        evaluated_windows: precheck.evaluated_windows,
        weak_positions: precheck.weak_positions.clone(),
    })?;

    for observation in fitting_evidence
        .observations()
        .iter()
        .filter(|observation| {
            observation.role() == EvidenceRole::Training
                && observation.domain() == EvidenceDomain::PassiveSurface
                && observation.position().is_some()
        })
    {
        app.apply_hardware_event(HardwareCalibrationEvent::ExampleAccepted {
            domain: CalibrationDomain::PassiveSurface,
            position: observation.position().expect("filter retained a position"),
            block: BlockId::new(observation.block_id()),
            capture_lease_revision,
        })?;
    }
    let validation_block = validation_evidence
        .block_ids()
        .into_iter()
        .next()
        .ok_or(CaptureCalibrationRunError::MissingValidationBlock)?;
    app.apply_hardware_event(HardwareCalibrationEvent::BeginArtifactFit(BlockId::new(
        validation_block,
    )))?;
    app.apply_hardware_event(HardwareCalibrationEvent::ArtifactLocked(
        fitted
            .artifact
            .locked_identity()
            .map_err(CalibrationEngineError::Artifact)?,
    ))?;
    app.apply_hardware_event(HardwareCalibrationEvent::ElapsedSeconds(
        validation_evidence.setup_seconds(),
    ))?;
    let snapshot = app.apply_hardware_event(HardwareCalibrationEvent::SurfaceValidation(
        validated.report.clone(),
    ))?;

    Ok(CaptureCalibrationRun {
        artifact: fitted.artifact,
        validation_report: validated.report,
        fitting_evidence: fitted.diagnostic_evidence,
        validation_evidence: validated.diagnostic_evidence,
        keyboard_precheck: precheck,
        snapshot,
    })
}

#[derive(Debug)]
pub enum CaptureCalibrationRunError {
    App(CalibrationAppError),
    Engine(CalibrationEngineError),
    MissingThresholdBlock,
    MissingValidationBlock,
    ElapsedTimeRegression,
}

impl fmt::Display for CaptureCalibrationRunError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::App(error) => write!(formatter, "calibration application failed: {error}"),
            Self::Engine(error) => write!(formatter, "calibration evidence failed: {error}"),
            Self::MissingThresholdBlock => {
                formatter.write_str("calibration evidence has no keyboard precheck block")
            }
            Self::MissingValidationBlock => {
                formatter.write_str("calibration evidence has no held-out validation block")
            }
            Self::ElapsedTimeRegression => formatter
                .write_str("held-out calibration time is earlier than the fitting checkpoint"),
        }
    }
}

impl Error for CaptureCalibrationRunError {}

impl From<CalibrationAppError> for CaptureCalibrationRunError {
    fn from(error: CalibrationAppError) -> Self {
        Self::App(error)
    }
}

impl From<CalibrationEngineError> for CaptureCalibrationRunError {
    fn from(error: CalibrationEngineError) -> Self {
        Self::Engine(error)
    }
}

fn advance_fixture(coordinator: &mut CalibrationCoordinator) -> Result<(), CoordinatorError> {
    match coordinator.phase() {
        CalibrationPhase::Disconnected => coordinator.connect(),
        CalibrationPhase::HardwareCheck => coordinator.accept_hardware_check(),
        CalibrationPhase::KeyboardIdentify => {
            coordinator.identify_keyboard(KeyboardLayout::UsQwerty)
        }
        CalibrationPhase::NeutralPose => coordinator.accept_neutral_pose(),
        CalibrationPhase::KeyboardCollect => {
            for slot in all_slots()
                .iter()
                .filter(|slot| keyboard_warm_start_eligible(slot.position))
            {
                for _ in 0..4 {
                    coordinator.accept_example(
                        CalibrationDomain::Keyboard,
                        slot.position,
                        BlockId::new(10),
                    )?;
                }
                for _ in 0..4 {
                    coordinator.accept_example(
                        CalibrationDomain::Keyboard,
                        slot.position,
                        BlockId::new(11),
                    )?;
                }
            }
            coordinator.begin_keyboard_precheck(BlockId::new(20))
        }
        CalibrationPhase::KeyboardValidate => coordinator.submit_keyboard_precheck(
            KeyboardPrecheckMetrics::new(9_500, 9_000).expect("fixture metrics are valid"),
            &[],
        ),
        CalibrationPhase::SurfaceCollect => {
            for slot in all_slots() {
                for _ in 0..3 {
                    coordinator.accept_example(
                        CalibrationDomain::PassiveSurface,
                        slot.position,
                        BlockId::new(30),
                    )?;
                }
            }
            coordinator.begin_artifact_fit(BlockId::new(40))
        }
        CalibrationPhase::FitLockedArtifact => coordinator.lock_artifact(
            LockedArtifactIdentity::new([0xa1; 32], [0xb1; 32], vec![[0xc1; 32]])
                .expect("fixture identity is complete"),
        ),
        CalibrationPhase::SurfaceValidate => {
            let report = passing_fixture_validation();
            coordinator.submit_surface_validation(&report)
        }
        CalibrationPhase::ValidationPassed
        | CalibrationPhase::NeedsHardwareAttention
        | CalibrationPhase::CalibrationFailed
        | CalibrationPhase::Aborted => Ok(()),
    }
}

fn passing_fixture_validation() -> ValidationReport {
    let mut confusion = ValidationMatrix::new();
    for slot in all_slots() {
        for _ in 0..3 {
            confusion.observe(
                TruthClass::Position(slot.position),
                slot.usage
                    .map_or(PredictionClass::Null, PredictionClass::Key),
            );
        }
    }
    for _ in 0..20 {
        confusion.observe(TruthClass::Null, PredictionClass::Null);
    }
    ValidationReport::new(ValidationReportInput {
        artifact_sha256: [0xa1; 32],
        acquisition_manifest_sha256: [0xb1; 32],
        evaluator_version: "simulated-surface-evaluator-v1".to_owned(),
        session_sha256: vec![[0xd1; 32]],
        evidence_sha256: vec![[0xd2; 32]],
        block_ids: vec![40],
        confusion,
        ordinary_motion_exposure_ms: 60_000,
        key_state_neutral: true,
    })
    .expect("fixture validation is structurally valid")
}

fn snapshot_from_state(state: &CalibrationAppState) -> CalibrationSnapshot {
    let progress = state.coordinator.progress();
    let phase = state.coordinator.phase();
    let completed = u64::from(progress.keyboard_accepted) + u64::from(progress.surface_accepted);
    let required = u64::from(progress.keyboard_required) + u64::from(progress.surface_required);
    let base_progress = if required == 0 {
        0
    } else {
        u16::try_from(completed.saturating_mul(9_000) / required)
            .unwrap_or(9_000)
            .min(9_000)
    };
    let setup_progress_basis_points = match phase {
        CalibrationPhase::Disconnected => 0,
        CalibrationPhase::HardwareCheck => 500,
        CalibrationPhase::KeyboardIdentify => 1_000,
        CalibrationPhase::NeutralPose => 1_500,
        CalibrationPhase::KeyboardCollect | CalibrationPhase::KeyboardValidate => {
            base_progress.max(2_000)
        }
        CalibrationPhase::SurfaceCollect | CalibrationPhase::FitLockedArtifact => {
            base_progress.max(6_000)
        }
        CalibrationPhase::SurfaceValidate => 9_900,
        CalibrationPhase::ValidationPassed => 10_000,
        CalibrationPhase::NeedsHardwareAttention
        | CalibrationPhase::CalibrationFailed
        | CalibrationPhase::Aborted => base_progress,
    };
    let prompt_attempt = progress
        .keyboard_accepted
        .saturating_add(progress.rejected_examples);
    let prompt = state
        .keyboard_prompt
        .clone()
        .unwrap_or_else(|| keyboard_prompt(&state.coordinator, prompt_attempt));
    // Hardware instructions carry the exact position that the browser later
    // acknowledges. The deterministic replay has no browser-timed capture
    // instruction, so derive its display-only position from the coordinator.
    let surface_position = state.surface_position.or_else(|| {
        (state.mode == EvidenceMode::SimulatedReplay && phase == CalibrationPhase::SurfaceCollect)
            .then(|| state.coordinator.next_surface_position())
            .flatten()
    });
    let surface_target = state.surface_target.clone().unwrap_or_else(|| {
        surface_position
            .and_then(|position| {
                slot_for_position(position).map(|slot| format!("Tap {}", slot.label))
            })
            .unwrap_or_else(|| "Surface block complete".to_owned())
    });
    let capture_lease_revision = state
        .capture_lease
        .as_ref()
        .filter(|lease| !state.paused && Instant::now() <= lease.deadline)
        .map(|lease| lease.revision);
    let keyboard_coverage_basis_points = (progress.keyboard_required != 0).then(|| {
        u16::try_from(
            u64::from(progress.keyboard_accepted).saturating_mul(10_000)
                / u64::from(progress.keyboard_required),
        )
        .unwrap_or(10_000)
        .min(10_000)
    });
    let keyboard_precheck = state.keyboard_precheck.as_ref();
    let keyboard_weak_positions = keyboard_precheck.map_or_else(Vec::new, |feedback| {
        feedback
            .weak_positions
            .iter()
            .map(|position| {
                slot_for_position(*position)
                    .map_or_else(|| format!("{position:?}"), |slot| slot.label.to_owned())
            })
            .collect()
    });
    CalibrationSnapshot {
        revision: state.revision,
        capture_lease_revision,
        surface_instruction_revision: state.surface_instruction.pending_revision(),
        evidence: state.mode.label().to_owned(),
        limitation: state.mode.limitation().to_owned(),
        phase: phase_name(phase).to_owned(),
        screen: screen_for_phase(phase).to_owned(),
        hardware_health: if state.mode == EvidenceMode::SimulatedReplay {
            match phase {
                CalibrationPhase::Disconnected => "not checked",
                CalibrationPhase::HardwareCheck => "checking six sensors per hand",
                CalibrationPhase::NeedsHardwareAttention => "failed: reseat the named sensor",
                _ => "simulated fixture only",
            }
            .to_owned()
        } else {
            state.hardware_health.clone()
        },
        keyboard_device: if state.mode == EvidenceMode::SimulatedReplay {
            "Fixture USB keyboard (US QWERTY)"
        } else {
            &state.keyboard_device
        }
        .to_owned(),
        supported_keyboard_layout: format!("US QWERTY physical positions ({US_QWERTY_LAYOUT_ID})"),
        keyboard_layout_prompt_id: US_QWERTY_CONFIRMATION_PROMPT_ID.to_owned(),
        keyboard_layout_instruction_revision: state.keyboard_layout_decision.instruction_revision(),
        keyboard_layout_confirmation: if state.mode == EvidenceMode::SimulatedReplay {
            "simulated-fixture"
        } else {
            match &state.keyboard_layout_decision {
                KeyboardLayoutDecisionState::WaitingForDevice => "waiting-for-device",
                KeyboardLayoutDecisionState::Pending { .. } => "awaiting-user",
                KeyboardLayoutDecisionState::Confirmed(_) => "confirmed",
                KeyboardLayoutDecisionState::Rejected { .. } => "rejected",
            }
        }
        .to_owned(),
        keyboard_layout_confirmation_revision: match &state.keyboard_layout_decision {
            KeyboardLayoutDecisionState::Confirmed(receipt) => Some(receipt.instruction_revision),
            KeyboardLayoutDecisionState::Rejected {
                instruction_revision,
                ..
            } => Some(*instruction_revision),
            KeyboardLayoutDecisionState::WaitingForDevice
            | KeyboardLayoutDecisionState::Pending { .. } => None,
        },
        keyboard_layout_confirmation_host_time_ns: match &state.keyboard_layout_decision {
            KeyboardLayoutDecisionState::Confirmed(receipt) => {
                Some(receipt.acknowledged_host_time_ns)
            }
            KeyboardLayoutDecisionState::Rejected {
                acknowledged_host_time_ns,
                ..
            } => Some(*acknowledged_host_time_ns),
            KeyboardLayoutDecisionState::WaitingForDevice
            | KeyboardLayoutDecisionState::Pending { .. } => None,
        },
        prompt,
        surface_target,
        surface_position: surface_position.map(|position| {
            slot_for_position(position)
                .expect("validated surface position belongs to the layout")
                .label
                .to_owned()
        }),
        neutral_seconds: state.neutral_seconds,
        elapsed_seconds: progress.elapsed_seconds,
        keyboard_accepted: progress.keyboard_accepted,
        keyboard_required: progress.keyboard_required,
        keyboard_coverage_basis_points,
        keyboard_intent_separation_basis_points: keyboard_precheck
            .map(|feedback| feedback.metrics.intent_separation_basis_points),
        keyboard_finger_agreement_basis_points: keyboard_precheck
            .map(|feedback| feedback.metrics.finger_agreement_basis_points),
        keyboard_precheck_evaluated_windows: keyboard_precheck
            .map_or(0, |feedback| feedback.evaluated_windows),
        keyboard_weak_positions,
        keyboard_precheck: keyboard_precheck.map_or_else(
            || "No evidence-backed keyboard check has run.".to_owned(),
            |feedback| {
                if feedback.weak_positions.is_empty() {
                    "passed keyboard signal checks".to_owned()
                } else {
                    "needs more keyboard examples for named positions".to_owned()
                }
            },
        ),
        keyboard_accuracy: "Key-identity accuracy is not available from the keyboard precheck."
            .to_owned(),
        surface_accepted: progress.surface_accepted,
        surface_required: progress.surface_required,
        rejected_examples: progress.rejected_examples,
        setup_progress_basis_points,
        validation: match phase {
            CalibrationPhase::SurfaceValidate => "running",
            CalibrationPhase::ValidationPassed => "passed",
            CalibrationPhase::NeedsHardwareAttention | CalibrationPhase::CalibrationFailed => {
                "failed"
            }
            _ => "not-run",
        }
        .to_owned(),
        heldout_validation_passed: phase == CalibrationPhase::ValidationPassed,
        failure_cause: state
            .coordinator
            .failure()
            .map(calibration_failure_code)
            .map(str::to_owned),
        result_acknowledgement: match state.result_acknowledgement {
            ResultAcknowledgementState::Unavailable => "unavailable",
            ResultAcknowledgementState::Pending(_) => "pending",
            ResultAcknowledgementState::Acknowledged(_) => "acknowledged",
        }
        .to_owned(),
        result_acknowledgement_revision: state.result_acknowledgement.instruction_revision(),
        result_acknowledgement_host_time_ns: match state.result_acknowledgement {
            ResultAcknowledgementState::Acknowledged(receipt) => {
                Some(receipt.acknowledged_host_time_ns)
            }
            ResultAcknowledgementState::Unavailable | ResultAcknowledgementState::Pending(_) => {
                None
            }
        },
        paused: state.paused,
        last_error: state.last_error.clone(),
    }
}

fn keyboard_prompt(coordinator: &CalibrationCoordinator, attempt: u32) -> String {
    let Some(position) = coordinator.next_keyboard_position() else {
        return "Keyboard block complete".to_owned();
    };
    let mut deficits = [0_u16; PHYSICAL_POSITION_COUNT];
    if let Some(index) = position_index(position) {
        deficits[index] = 1;
    }
    PromptCorpus::v1()
        .next_keyboard_prompt(&deficits, attempt)
        .instruction
        .to_owned()
}

const fn screen_for_phase(phase: CalibrationPhase) -> &'static str {
    match phase {
        CalibrationPhase::Disconnected => "connect",
        CalibrationPhase::HardwareCheck => "sensor-check",
        CalibrationPhase::KeyboardIdentify => "identify-keyboard",
        CalibrationPhase::NeutralPose => "neutral-pose",
        CalibrationPhase::KeyboardCollect | CalibrationPhase::KeyboardValidate => "keyboard-typing",
        CalibrationPhase::SurfaceCollect
        | CalibrationPhase::FitLockedArtifact
        | CalibrationPhase::SurfaceValidate => "surface-transfer",
        CalibrationPhase::ValidationPassed
        | CalibrationPhase::NeedsHardwareAttention
        | CalibrationPhase::CalibrationFailed
        | CalibrationPhase::Aborted => "validation-result",
    }
}

const fn phase_name(phase: CalibrationPhase) -> &'static str {
    match phase {
        CalibrationPhase::Disconnected => "disconnected",
        CalibrationPhase::HardwareCheck => "hardware-check",
        CalibrationPhase::KeyboardIdentify => "keyboard-identify",
        CalibrationPhase::NeutralPose => "neutral-pose",
        CalibrationPhase::KeyboardCollect => "keyboard-collect",
        CalibrationPhase::KeyboardValidate => "keyboard-validate",
        CalibrationPhase::SurfaceCollect => "surface-collect",
        CalibrationPhase::FitLockedArtifact => "fit-locked-artifact",
        CalibrationPhase::SurfaceValidate => "surface-validate",
        CalibrationPhase::ValidationPassed => "validation-passed",
        CalibrationPhase::NeedsHardwareAttention | CalibrationPhase::CalibrationFailed => {
            "calibration-failed"
        }
        CalibrationPhase::Aborted => "aborted",
    }
}

/// Stable machine-readable cause exposed by [`CalibrationSnapshot`].
#[must_use]
pub const fn calibration_failure_code(failure: CalibrationFailure) -> &'static str {
    match failure {
        CalibrationFailure::HardwareFault => "hardware-fault",
        CalibrationFailure::UnsupportedLayout => "unsupported-layout",
        CalibrationFailure::SurfaceValidation => "heldout-validation-failed",
        CalibrationFailure::ValidationPlateau => "validation-plateau",
        CalibrationFailure::ExampleCap => "example-cap",
        CalibrationFailure::TimeCap => "time-cap",
    }
}

#[must_use]
pub fn frontend_html(token: &str) -> String {
    FRONTEND_HTML
        .replace("__CALIBRATION_TOKEN__", token)
        .replace(
            "__SUPPORTED_KEYBOARD_LAYOUT_COPY__",
            US_QWERTY_CONFIRMATION_COPY,
        )
        .replace(
            "__CORE28_LAYOUT_SHA256__",
            &format_sha256(&sha256(&core28_layout_manifest_bytes())),
        )
        .replace("__CORE28_TUTORIAL_LAYOUT__", &surface_layout_html(false))
        .replace("__CORE28_OWNERSHIP_LAYOUT__", &ownership_layout_html())
        .replace("__CORE28_TARGET_LAYOUT__", &surface_layout_html(true))
}

fn surface_layout_html(targetable: bool) -> String {
    use std::fmt::Write as _;

    let mut html = String::new();
    let row_count = all_slots()
        .iter()
        .map(|slot| slot.display_row)
        .max()
        .map_or(0, |row| row.saturating_add(1));
    let column_count = all_slots()
        .iter()
        .map(|slot| slot.display_column)
        .max()
        .map_or(0, |column| column.saturating_add(1));
    for row in 0..row_count {
        html.push_str("<div class=\"keyboard-row\" role=\"row\">");
        for column in 0..column_count {
            let Some(slot) = all_slots()
                .iter()
                .find(|slot| slot.display_row == row && slot.display_column == column)
            else {
                html.push_str(
                    "<span class=\"keyboard-position empty\" aria-hidden=\"true\"></span>",
                );
                continue;
            };
            let label = slot.label;
            let hand = hand_name(slot.position.hand);
            let finger = finger_name(slot.position.finger);
            let reserved_class = if slot.usage.is_none() {
                " reserved"
            } else {
                ""
            };
            let target_attribute = if targetable {
                format!(" data-position=\"{label}\"")
            } else {
                String::new()
            };
            let output = if slot.usage.is_none() {
                "<span class=\"position-output\">No key</span>"
            } else {
                ""
            };
            let spoken_label = surface_position_spoken_label(label);
            let behavior = if slot.usage.is_none() {
                "reserved negative example, emits no key"
            } else {
                "enabled key"
            };
            write!(
                html,
                "<span class=\"keyboard-position{reserved_class}\" role=\"gridcell\"{target_attribute} aria-label=\"{spoken_label} position, {hand} {finger}, {behavior}\"><span class=\"position-label\">{}</span><span class=\"position-owner\">{hand} {finger}</span>{output}</span>",
                surface_position_text(label),
            )
            .expect("writing to a String cannot fail");
        }
        html.push_str("</div>");
    }
    html
}

fn ownership_layout_html() -> String {
    use std::fmt::Write as _;

    let mut html = String::new();
    for hand in [Hand::Left, Hand::Right] {
        let hand_label = hand_name(hand);
        write!(
            html,
            "<section aria-labelledby=\"{hand_label}-ownership-title\"><h4 id=\"{hand_label}-ownership-title\">{} hand</h4><dl class=\"ownership-list\">",
            match hand {
                Hand::Left => "Left",
                Hand::Right => "Right",
            }
        )
        .expect("writing to a String cannot fail");
        for finger in [
            Finger::Pinky,
            Finger::Ring,
            Finger::Middle,
            Finger::Index,
            Finger::Thumb,
        ] {
            let labels = all_slots()
                .iter()
                .filter(|slot| slot.position.hand == hand && slot.position.finger == finger)
                .map(|slot| {
                    let label = surface_position_text(slot.label);
                    if slot.usage.is_none() {
                        format!("{label} (reserved)")
                    } else {
                        label.to_owned()
                    }
                })
                .collect::<Vec<_>>()
                .join(", ");
            write!(html, "<dt>{}</dt><dd>{labels}</dd>", finger_name(finger))
                .expect("writing to a String cannot fail");
        }
        html.push_str("</dl></section>");
    }
    html
}

const fn hand_name(hand: Hand) -> &'static str {
    match hand {
        Hand::Left => "left",
        Hand::Right => "right",
    }
}

const fn finger_name(finger: Finger) -> &'static str {
    match finger {
        Finger::Thumb => "thumb",
        Finger::Index => "index",
        Finger::Middle => "middle",
        Finger::Ring => "ring",
        Finger::Pinky => "pinky",
    }
}

fn surface_position_spoken_label(label: &str) -> &str {
    match label {
        "semicolon" => "semicolon",
        "comma" => "comma",
        "period" => "period",
        "slash" => "slash",
        "left-outer-thumb" => "left outer thumb",
        "right-outer-thumb" => "right outer thumb",
        _ => label,
    }
}

fn surface_position_text(label: &str) -> &str {
    match label {
        "semicolon" => ";",
        "comma" => ",",
        "period" => ".",
        "slash" => "/",
        "left-outer-thumb" | "right-outer-thumb" => "Outer",
        _ => label,
    }
}

pub struct CalibrationHttpServer {
    listener: TcpListener,
    app: CalibrationApp,
    token: String,
    origin: String,
    host_clock: Arc<dyn MonotonicHostClock + Send + Sync>,
}

impl CalibrationHttpServer {
    pub fn bind(app: CalibrationApp, port: u16) -> Result<Self, CalibrationAppError> {
        Self::bind_with_host_clock(app, port, HostTimeOrigin::new())
    }

    /// Bind the UI service to the capture runner's monotonic clock origin.
    ///
    /// Hardware capture must pass the same cloned origin used to timestamp
    /// controller and keyboard input so browser receipts share that timeline.
    pub fn bind_with_host_clock<C>(
        app: CalibrationApp,
        port: u16,
        host_clock: C,
    ) -> Result<Self, CalibrationAppError>
    where
        C: MonotonicHostClock + Send + Sync + 'static,
    {
        let listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port))?;
        listener.set_nonblocking(true)?;
        let local_addr = listener.local_addr()?;
        Ok(Self {
            listener,
            app,
            token: random_token()?,
            origin: format!("http://127.0.0.1:{}", local_addr.port()),
            host_clock: Arc::new(host_clock),
        })
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    /// Serve one available request and return immediately when none is ready.
    pub fn poll_once(&self) -> Result<bool, CalibrationAppError> {
        match self.accept_connection() {
            Ok((mut stream, accepted_host_time_ns)) => {
                self.handle_connection(&mut stream, accepted_host_time_ns)?;
                Ok(true)
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    #[must_use]
    pub fn token(&self) -> &str {
        &self.token
    }

    pub fn serve_n_requests(self, count: usize) -> Result<(), CalibrationAppError> {
        self.listener.set_nonblocking(false)?;
        for _ in 0..count {
            let (mut stream, accepted_host_time_ns) = self.accept_connection()?;
            self.handle_connection(&mut stream, accepted_host_time_ns)?;
        }
        Ok(())
    }

    pub fn serve_until(
        self,
        stop: &std::sync::atomic::AtomicBool,
    ) -> Result<(), CalibrationAppError> {
        use std::{sync::atomic::Ordering, thread};

        while !stop.load(Ordering::SeqCst) {
            match self.accept_connection() {
                Ok((mut stream, accepted_host_time_ns)) => {
                    self.handle_connection(&mut stream, accepted_host_time_ns)?;
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(20));
                }
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }

    fn accept_connection(&self) -> io::Result<(TcpStream, u64)> {
        let (stream, _) = self.listener.accept()?;
        // Keep accept and timestamping in one operation so every server loop
        // records the same provenance boundary before request parsing.
        Ok((stream, self.host_clock.now_ns()))
    }

    fn handle_connection(
        &self,
        stream: &mut TcpStream,
        accepted_host_time_ns: u64,
    ) -> Result<(), CalibrationAppError> {
        match self.handle(stream, accepted_host_time_ns) {
            // All stream I/O after accept belongs to this connection. A peer
            // reset or refusal to read a response must not stop calibration.
            Err(CalibrationAppError::Io(_)) => Ok(()),
            result => result,
        }
    }

    fn handle(
        &self,
        stream: &mut TcpStream,
        accepted_host_time_ns: u64,
    ) -> Result<(), CalibrationAppError> {
        let request_deadline = Instant::now() + HTTP_CONNECTION_TIMEOUT;
        // A malformed client request belongs to that connection. It must not
        // escape the accept loop and terminate an in-progress calibration.
        let response = match read_request(stream, request_deadline) {
            Ok(request) => self.route(&request, accepted_host_time_ns),
            Err(CalibrationAppError::MalformedHttp(message)) => {
                HttpResponse::bad_request(format!("malformed HTTP request: {message}"))
            }
            Err(CalibrationAppError::Io(error))
                if matches!(
                    error.kind(),
                    io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                ) =>
            {
                HttpResponse::request_timeout()
            }
            Err(error) => return Err(error),
        };
        write_response(stream, response)
    }

    fn route(&self, request: &HttpRequest, accepted_host_time_ns: u64) -> HttpResponse {
        match (request.method.as_str(), request.path.as_str()) {
            ("GET", "/") => HttpResponse::ok(
                "text/html; charset=utf-8",
                frontend_html(&self.token).into_bytes(),
            ),
            ("GET", "/app.js") => HttpResponse::ok(
                "text/javascript; charset=utf-8",
                FRONTEND_JS.as_bytes().to_vec(),
            ),
            ("GET", "/style.css") => {
                HttpResponse::ok("text/css; charset=utf-8", FRONTEND_CSS.as_bytes().to_vec())
            }
            ("GET", "/api/state") => {
                if !self.authorized(request) {
                    return HttpResponse::forbidden();
                }
                match self.app.snapshot().and_then(|snapshot| {
                    serde_json::to_vec(&snapshot)
                        .map_err(|error| CalibrationAppError::Json(error.to_string()))
                }) {
                    Ok(body) => HttpResponse::ok("application/json", body),
                    Err(error) => HttpResponse::error(error.to_string()),
                }
            }
            ("POST", "/api/action") => {
                if !self.authorized(request)
                    || request.headers.get("origin") != Some(&self.origin)
                    || !request
                        .headers
                        .get("content-type")
                        .is_some_and(|value| value.starts_with("application/json"))
                {
                    return HttpResponse::forbidden();
                }
                let action = match serde_json::from_slice::<ActionRequest>(&request.body) {
                    Ok(action) => action,
                    Err(error) => return HttpResponse::bad_request(error.to_string()),
                };
                let action_result =
                    self.app
                        .apply_action(action.action, action.revision, accepted_host_time_ns);
                match action_result.and_then(|snapshot| {
                    serde_json::to_vec(&snapshot)
                        .map_err(|error| CalibrationAppError::Json(error.to_string()))
                }) {
                    Ok(body) => HttpResponse::ok("application/json", body),
                    Err(error) => HttpResponse::bad_request(error.to_string()),
                }
            }
            (_, "/api/state" | "/api/action") => HttpResponse::method_not_allowed(),
            _ => HttpResponse::not_found(),
        }
    }

    fn authorized(&self, request: &HttpRequest) -> bool {
        request.headers.get("x-calibration-token") == Some(&self.token)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ActionRequest {
    action: CalibrationAction,
    revision: u64,
}

struct HttpRequest {
    method: String,
    path: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

fn remaining_http_time(deadline: Instant) -> io::Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|remaining| !remaining.is_zero())
        .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "HTTP connection deadline expired"))
}

fn read_request(
    stream: &mut TcpStream,
    deadline: Instant,
) -> Result<HttpRequest, CalibrationAppError> {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 4_096];
    let (header_end, content_length) = loop {
        stream.set_read_timeout(Some(remaining_http_time(deadline)?))?;
        let read = stream.read(&mut buffer)?;
        remaining_http_time(deadline)?;
        if read == 0 {
            return Err(CalibrationAppError::MalformedHttp(
                "request ended before headers",
            ));
        }
        bytes.extend_from_slice(&buffer[..read]);
        if bytes.len() > MAX_HTTP_REQUEST_BYTES {
            return Err(CalibrationAppError::MalformedHttp("request is too large"));
        }
        if let Some(header_end) = find_header_end(&bytes) {
            let header_text = std::str::from_utf8(&bytes[..header_end])
                .map_err(|_| CalibrationAppError::MalformedHttp("headers are not UTF-8"))?;
            let content_length = parse_content_length(header_text)?;
            if content_length > MAX_HTTP_BODY_BYTES {
                return Err(CalibrationAppError::MalformedHttp(
                    "request body is too large",
                ));
            }
            if bytes.len() >= header_end + 4 + content_length {
                break (header_end, content_length);
            }
        }
    };

    let header_text = std::str::from_utf8(&bytes[..header_end])
        .map_err(|_| CalibrationAppError::MalformedHttp("headers are not UTF-8"))?;
    let mut lines = header_text.split("\r\n");
    let request_line = lines.next().ok_or(CalibrationAppError::MalformedHttp(
        "request line is missing",
    ))?;
    let mut parts = request_line.split_whitespace();
    let method = parts
        .next()
        .ok_or(CalibrationAppError::MalformedHttp("method is missing"))?;
    let path = parts
        .next()
        .ok_or(CalibrationAppError::MalformedHttp("path is missing"))?;
    let version = parts.next().ok_or(CalibrationAppError::MalformedHttp(
        "HTTP version is missing",
    ))?;
    if parts.next().is_some() || version != "HTTP/1.1" || !path.starts_with('/') {
        return Err(CalibrationAppError::MalformedHttp(
            "request line is invalid",
        ));
    }
    let mut headers = BTreeMap::new();
    for line in lines {
        let (name, value) = line
            .split_once(':')
            .ok_or(CalibrationAppError::MalformedHttp("header is invalid"))?;
        let name = name.trim().to_ascii_lowercase();
        if headers.insert(name, value.trim().to_owned()).is_some() {
            return Err(CalibrationAppError::MalformedHttp("duplicate header"));
        }
    }
    let body_start = header_end + 4;
    Ok(HttpRequest {
        method: method.to_owned(),
        path: path.to_owned(),
        headers,
        body: bytes[body_start..body_start + content_length].to_vec(),
    })
}

fn find_header_end(bytes: &[u8]) -> Option<usize> {
    bytes.windows(4).position(|window| window == b"\r\n\r\n")
}

fn parse_content_length(headers: &str) -> Result<usize, CalibrationAppError> {
    for line in headers.split("\r\n").skip(1) {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.trim().eq_ignore_ascii_case("content-length") {
            return value
                .trim()
                .parse()
                .map_err(|_| CalibrationAppError::MalformedHttp("content length is invalid"));
        }
    }
    Ok(0)
}

struct HttpResponse {
    status: &'static str,
    content_type: &'static str,
    body: Vec<u8>,
}

impl HttpResponse {
    fn ok(content_type: &'static str, body: Vec<u8>) -> Self {
        Self {
            status: "200 OK",
            content_type,
            body,
        }
    }

    fn bad_request(message: String) -> Self {
        Self::plain("400 Bad Request", message)
    }

    fn forbidden() -> Self {
        Self::plain("403 Forbidden", "forbidden".to_owned())
    }

    fn request_timeout() -> Self {
        Self::plain("408 Request Timeout", "request timed out".to_owned())
    }

    fn not_found() -> Self {
        Self::plain("404 Not Found", "not found".to_owned())
    }

    fn method_not_allowed() -> Self {
        Self::plain("405 Method Not Allowed", "method not allowed".to_owned())
    }

    fn error(message: String) -> Self {
        Self::plain("500 Internal Server Error", message)
    }

    fn plain(status: &'static str, message: String) -> Self {
        Self {
            status,
            content_type: "text/plain; charset=utf-8",
            body: message.into_bytes(),
        }
    }
}

struct HttpResponseWriter<'a> {
    stream: &'a mut TcpStream,
    deadline: Instant,
}

impl Write for HttpResponseWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.stream
            .set_write_timeout(Some(remaining_http_time(self.deadline)?))?;
        self.stream.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        // TcpStream writes directly to the socket; there is no userspace buffer.
        Ok(())
    }
}

fn write_response(
    stream: &mut TcpStream,
    response: HttpResponse,
) -> Result<(), CalibrationAppError> {
    // Share one deadline across formatted headers and every partial body write.
    // Keep a separate response budget so a timed-out request can receive 408.
    let mut writer = HttpResponseWriter {
        stream,
        deadline: Instant::now() + HTTP_CONNECTION_TIMEOUT,
    };
    write!(
        writer,
        "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\nContent-Security-Policy: default-src 'self'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'none'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'\r\nReferrer-Policy: no-referrer\r\nX-Content-Type-Options: nosniff\r\n\r\n",
        response.status,
        response.content_type,
        response.body.len()
    )?;
    writer.write_all(&response.body)?;
    Ok(())
}

fn random_token() -> Result<String, CalibrationAppError> {
    let mut bytes = [0_u8; 24];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(format_lower_hex(&bytes))
}

#[derive(Debug)]
pub enum CalibrationAppError {
    Coordinator(CoordinatorError),
    Io(io::Error),
    Json(String),
    UnknownAction(String),
    ActionNotAllowed,
    Paused,
    CaptureLeaseRequired,
    CaptureLeaseAlreadyActive,
    StaleCaptureLease,
    InvalidInstruction,
    KeyboardLayoutInstructionNotPending,
    KeyboardLayoutDecisionConflict,
    SurfaceInstructionNotPending,
    ResultAcknowledgementNotPending,
    SavedResultReadOnly,
    StaleStateRevision { expected: u64, supplied: u64 },
    StatePoisoned,
    MalformedHttp(&'static str),
}

impl fmt::Display for CalibrationAppError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Coordinator(error) => write!(formatter, "calibration state failed: {error}"),
            Self::Io(error) => write!(formatter, "calibration service I/O failed: {error}"),
            Self::Json(error) => write!(formatter, "calibration JSON failed: {error}"),
            Self::UnknownAction(action) => write!(formatter, "unknown calibration action {action}"),
            Self::ActionNotAllowed => {
                formatter.write_str("action is not allowed in the current evidence mode")
            }
            Self::Paused => formatter.write_str("calibration is paused"),
            Self::CaptureLeaseRequired => {
                formatter.write_str("calibration has no active capture lease")
            }
            Self::CaptureLeaseAlreadyActive => {
                formatter.write_str("calibration capture lease is already active")
            }
            Self::StaleCaptureLease => {
                formatter.write_str("stale capture lease or expired capture lease")
            }
            Self::InvalidInstruction => {
                formatter.write_str("calibration capture instruction is invalid")
            }
            Self::KeyboardLayoutInstructionNotPending => {
                formatter.write_str("keyboard layout instruction is not pending for this revision")
            }
            Self::KeyboardLayoutDecisionConflict => {
                formatter.write_str("keyboard layout decision cannot change after it is recorded")
            }
            Self::SurfaceInstructionNotPending => {
                formatter.write_str("surface instruction is not pending for this rendered revision")
            }
            Self::ResultAcknowledgementNotPending => {
                formatter.write_str("saved result acknowledgement is not pending for this revision")
            }
            Self::SavedResultReadOnly => {
                formatter.write_str("saved calibration result is read-only")
            }
            Self::StaleStateRevision { expected, supplied } => write!(
                formatter,
                "calibration state revision is stale: expected {expected}, supplied {supplied}"
            ),
            Self::StatePoisoned => formatter.write_str("calibration state lock is poisoned"),
            Self::MalformedHttp(message) => write!(formatter, "malformed HTTP request: {message}"),
        }
    }
}

impl Error for CalibrationAppError {}

impl From<CoordinatorError> for CalibrationAppError {
    fn from(error: CoordinatorError) -> Self {
        Self::Coordinator(error)
    }
}

impl From<io::Error> for CalibrationAppError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}
