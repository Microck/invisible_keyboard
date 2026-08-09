//! Linux hardware calibration service with one authoritative evidence owner.
//!
//! The browser renders instructions and holds a short capture lease. Device
//! readers timestamp raw facts on one monotonic clock. This service is the only
//! component allowed to join those facts, advance evidence checkpoints, fit a
//! model, or declare held-out validation passed.

#![cfg(target_os = "linux")]

use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fs::{File, OpenOptions},
    io::{self, Read, Write},
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, TryLockError,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use crate::{
    artifact::{CalibrationArtifact, KeyboardLayoutConfirmationReceipt},
    calibration::{
        BlockId, CalibrationDomain, CalibrationFailure, CalibrationPhase,
        KEYBOARD_EXAMPLES_PER_POSITION, MAX_EXAMPLES_PER_POSITION,
    },
    calibration_app::{
        CalibrationApp, CalibrationControlState, CalibrationHttpServer, EvidenceMode,
        HardwareCalibrationEvent, KeyboardLayoutDecisionState, ResultAcknowledgementReceipt,
    },
    calibration_capture::{
        CalibrationCaptureError, CalibrationSurfacePlanBinding, SurfacePresentationReceipt,
    },
    calibration_engine::{
        CalibrationFitInput, CalibrationValidationResult, EvidenceDomain, EvidencePurpose,
        EvidenceRole, evaluate_keyboard_precheck, fit_calibration, validate_calibration,
    },
    calibration_worker::{
        CalibrationCaptureBundle, CalibrationCaptureIncident, CalibrationCaptureWorker,
        CalibrationCaptureWorkerConfig, CalibrationIncidentReason, CalibrationWorkerError,
        KeyboardCaptureContext, KeyboardEvidenceCheckpoint, MatchedKeyboardObservation,
    },
    controller_source::open_controller_source,
    device_transport::{
        DeviceTransportConfig, DeviceTransportEvent, DeviceWireIdentityBootstrap,
        IdentityBootstrapConfig,
    },
    labels::{
        HostTimeOrigin, KeyboardEventSource, KeyboardSourceIdentity, LabelDisposition,
        LinuxKeyboardDevice, RawKeyState, RawKeyboardEvent,
    },
    layout::{
        Hand, PHYSICAL_POSITION_COUNT, PhysicalPosition, all_slots, keyboard_warm_start_eligible,
        position_index, slot_for_position,
    },
    manifest::{AcquisitionManifest, AcquisitionManifestTemplate, format_lower_hex},
    neutral::{NeutralRecordWindow, NeutralWindowRole, StabilityVerdict},
    pipeline::WINDOW_FRAMES,
    prompts::{
        HeldOutSurfacePlan, Prompt, PromptCorpus, PromptId, SurfacePresentationPhase,
        SurfacePromptSegment, SurfaceSegmentKind, SurfaceTargetMetadata, SurfaceTrainingPlan,
    },
};

const DEVICE_CHANNEL_CAPACITY: usize = 256;
const READ_BUFFER_BYTES: usize = 4_096;
const LOOP_WAIT: Duration = Duration::from_millis(5);
const HEALTH_CHECK_DURATION_US: u64 = 5_000_000;
const NEUTRAL_HOLD_NS: u64 = 3_000_000_000;
const PLAN_LEAD_IN_NS: u64 = 2_000_000_000;
const PLAN_DRAIN_NS: u64 = 500_000_000;
pub const RESULT_ACKNOWLEDGEMENT_TIMEOUT: Duration = Duration::from_secs(5 * 60);
pub const HARDWARE_CALIBRATION_FINAL_STATE_SCHEMA: &str =
    "invisible-keyboard-hardware-calibration-final-state-v1";

pub type HardwareCalibrationError = Box<dyn Error + Send + Sync>;

/// Paths and local UI port for one independently implemented calibration attempt.
pub struct HardwareCalibrationConfig {
    pub template: AcquisitionManifestTemplate,
    pub left_device: PathBuf,
    pub right_device: PathBuf,
    pub keyboard_device: PathBuf,
    pub output_directory: PathBuf,
    pub port: u16,
    pub stop: Arc<AtomicBool>,
}

/// Final persisted identities returned after the held-out gate finishes.
pub struct HardwareCalibrationOutcome {
    pub output_directory: PathBuf,
    pub artifact_sha256: [u8; 32],
    pub validation_report_sha256: [u8; 32],
    pub result_acknowledgement: HardwareCalibrationResultAcknowledgement,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResultAcknowledgementDecision {
    Waiting,
    Acknowledged(ResultAcknowledgementReceipt),
    TimedOut,
}

/// Pure post-result deadline decision used by the runner and contract tests.
#[must_use]
pub fn result_acknowledgement_decision(
    elapsed: Duration,
    receipt: Option<ResultAcknowledgementReceipt>,
) -> ResultAcknowledgementDecision {
    if let Some(receipt) = receipt {
        ResultAcknowledgementDecision::Acknowledged(receipt)
    } else if elapsed >= RESULT_ACKNOWLEDGEMENT_TIMEOUT {
        ResultAcknowledgementDecision::TimedOut
    } else {
        ResultAcknowledgementDecision::Waiting
    }
}

#[derive(Clone, Copy, Debug, serde::Deserialize, Eq, PartialEq, serde::Serialize)]
#[serde(tag = "status", rename_all = "kebab-case", deny_unknown_fields)]
pub enum HardwareCalibrationResultAcknowledgement {
    Acknowledged {
        instruction_revision: u64,
        acknowledged_host_time_ns: u64,
    },
    TimedOut {
        instruction_revision: u64,
        timeout_seconds: u64,
    },
}

/// Typed cause committed for every attempt that does not pass held-out validation.
pub type HardwareCalibrationTerminalCause = CalibrationIncidentReason;

#[derive(Clone, Debug)]
struct TerminalFailure {
    cause: HardwareCalibrationTerminalCause,
    detail: String,
}

impl TerminalFailure {
    fn new(cause: HardwareCalibrationTerminalCause, detail: impl Into<String>) -> Self {
        Self {
            cause,
            detail: detail.into(),
        }
    }
}

/// Durable commit marker for one hardware calibration attempt.
#[derive(Clone, Debug, serde::Deserialize, Eq, PartialEq, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct HardwareCalibrationFinalState {
    pub schema: String,
    pub heldout_validation_passed: bool,
    pub completed_unix_ns: u64,
    pub disposition: HardwareCalibrationFinalDisposition,
}

#[derive(Clone, Debug, serde::Deserialize, Eq, PartialEq, serde::Serialize)]
#[serde(tag = "status", rename_all = "kebab-case", deny_unknown_fields)]
pub enum HardwareCalibrationFinalDisposition {
    ValidationPassed {
        artifact_sha256: String,
        validation_report_sha256: String,
        result_acknowledgement: HardwareCalibrationResultAcknowledgement,
    },
    NonReady {
        cause: HardwareCalibrationTerminalCause,
        detail: String,
        incident_persisted: bool,
        incident_error: Option<String>,
    },
}

enum SensorInput {
    Chunk {
        source: Hand,
        receipt_time_ns: u64,
        bytes: Vec<u8>,
    },
    End {
        source: Hand,
        receipt_time_ns: u64,
    },
    Failed {
        source: Hand,
        receipt_time_ns: u64,
        error: io::Error,
    },
}

enum KeyboardInput {
    Event(RawKeyboardEvent),
    Failed(io::Error),
}

/// Key state owned by the one selected reader across capture-worker resets.
///
/// Workers keep raw event provenance for their own evidence interval. This
/// runner-level state prevents a held key from being forgotten when fitting or
/// validation swaps workers between those intervals.
#[derive(Default)]
struct SelectedKeyboardState {
    held_codes: BTreeSet<u16>,
}

impl SelectedKeyboardState {
    fn from_held_codes(held_codes: impl IntoIterator<Item = u16>) -> Self {
        Self {
            held_codes: held_codes.into_iter().collect(),
        }
    }

    fn observe(&mut self, event: &RawKeyboardEvent) {
        match event.state {
            RawKeyState::Down => {
                self.held_codes.insert(event.physical_code);
            }
            RawKeyState::Up => {
                self.held_codes.remove(&event.physical_code);
            }
            RawKeyState::Repeat => {}
        }
    }

    fn is_neutral(&self) -> bool {
        self.held_codes.is_empty()
    }
}

enum CaptureStage {
    HardwareCheck,
    KeyboardIdentification,
    AwaitLayoutDecision,
    Neutral {
        started_ns: u64,
        start_record: u64,
        displayed_seconds: u8,
    },
    KeyboardTraining(KeyboardTrainingState),
    SurfaceTraining(TimedSurfaceCapture),
    ArtifactFitting {
        result: Receiver<Result<FittedCalibration, HardwareCalibrationError>>,
    },
    HeldOutValidation(TimedSurfaceCapture),
    HeldOutValidating {
        result: Receiver<Result<ValidatedCalibration, HardwareCalibrationError>>,
    },
    Complete,
}

struct FittedCalibration {
    bundle: CalibrationCaptureBundle,
    artifact: CalibrationArtifact,
}

struct ValidatedCalibration {
    bundle: CalibrationCaptureBundle,
    validation: CalibrationValidationResult,
}

struct FrozenCapture {
    worker: Arc<CalibrationCaptureWorker>,
    prefix: &'static str,
    display_receipts: Vec<SurfaceDisplayReceipt>,
    display_receipts_persisted: bool,
}

struct TimedDisplay {
    at_ns: u64,
    instruction: String,
    highlighted_position: Option<PhysicalPosition>,
    prompted_position: Option<PhysicalPosition>,
    prompt_id: PromptId,
    phase: SurfacePresentationPhase,
    pending: Option<PendingSurfaceInstruction>,
}

struct PendingSurfaceInstruction {
    revision: u64,
    published_at_ns: u64,
}

struct TimedNull {
    at_ns: u64,
    prompt_id: PromptId,
    sample_ordinal: u32,
}

struct TimedSurfaceCapture {
    started_ns: u64,
    duration_ns: u64,
    displays: Vec<TimedDisplay>,
    nulls: Vec<TimedNull>,
    next_display: usize,
    next_null: usize,
    schedule_shift_ns: u64,
    display_receipts: Vec<SurfaceDisplayReceipt>,
    canonical_plan: Vec<u8>,
}

#[derive(serde::Serialize)]
struct SurfaceDisplayReceipt {
    prompt_id: PromptId,
    phase: SurfacePresentationPhase,
    planned_offset_ns: u64,
    published_at_ns: u64,
    acknowledged_at_ns: u64,
    instruction_revision: u64,
    prompted_position: Option<PhysicalPosition>,
}

struct KeyboardPromptState {
    instruction: &'static str,
    expected_positions: Vec<PhysicalPosition>,
    next_position: usize,
}

struct KeyboardTrainingState {
    raw_attempts: [u8; PHYSICAL_POSITION_COUNT],
    evidence: KeyboardEvidenceCheckpoint,
    blocks_by_code: BTreeMap<u16, u32>,
    prompt_attempt: u32,
    prompt: KeyboardPromptState,
    drain_after_ns: Option<u64>,
}

impl KeyboardTrainingState {
    fn new() -> Self {
        let evidence = KeyboardEvidenceCheckpoint::empty();
        let prompt = keyboard_prompt_state(&evidence, 0);
        Self {
            raw_attempts: [0; PHYSICAL_POSITION_COUNT],
            evidence,
            blocks_by_code: BTreeMap::new(),
            prompt_attempt: 0,
            prompt,
            drain_after_ns: None,
        }
    }

    fn candidate_attempt(&self, event: &RawKeyboardEvent) -> bool {
        if event.state != RawKeyState::Down || event.position != self.prompt.current() {
            return false;
        }
        event
            .position
            .and_then(position_index)
            .is_some_and(|index| {
                self.evidence.deficits(KEYBOARD_EXAMPLES_PER_POSITION)[index] != 0
                    && self.raw_attempts[index] < MAX_EXAMPLES_PER_POSITION
            })
    }

    fn capped_deficit(&self) -> Option<PhysicalPosition> {
        let deficits = self.evidence.deficits(KEYBOARD_EXAMPLES_PER_POSITION);
        all_slots().iter().enumerate().find_map(|(index, slot)| {
            (deficits[index] != 0 && self.raw_attempts[index] >= MAX_EXAMPLES_PER_POSITION)
                .then_some(slot.position)
        })
    }
}

impl KeyboardPromptState {
    fn new(prompt: Prompt) -> Self {
        Self {
            instruction: prompt.instruction,
            expected_positions: prompt.expected_positions,
            next_position: 0,
        }
    }

    fn current(&self) -> Option<PhysicalPosition> {
        self.expected_positions.get(self.next_position).copied()
    }

    fn accept(&mut self, position: PhysicalPosition) -> bool {
        if self.current() != Some(position) {
            return false;
        }
        self.next_position += 1;
        true
    }

    fn is_complete(&self) -> bool {
        self.next_position == self.expected_positions.len()
    }

    fn rendered(&self) -> String {
        self.current().map_or_else(
            || format!("{} Done.", self.instruction),
            |position| {
                format!(
                    "{} Next: {} ({}/{})",
                    self.instruction,
                    label_for(position),
                    self.next_position + 1,
                    self.expected_positions.len()
                )
            },
        )
    }
}

/// Started local service. `run` owns the attempt until pass, fail, or stop.
pub struct HardwareCalibrationRunner {
    app: CalibrationApp,
    server: CalibrationHttpServer,
    stop: Arc<AtomicBool>,
    clock: HostTimeOrigin,
    sensor_input: Receiver<SensorInput>,
    keyboard_input: Receiver<KeyboardInput>,
    keyboard_delivery: Arc<Mutex<()>>,
    keyboard_source: KeyboardSourceIdentity,
    selected_keyboard_state: SelectedKeyboardState,
    manifest: AcquisitionManifest,
    output_directory: PathBuf,
    worker: Option<CalibrationCaptureWorker>,
    frozen_capture: Option<FrozenCapture>,
    stage: CaptureStage,
    capture_lease_revision: Option<u64>,
    keyboard_layout_confirmation: Option<KeyboardLayoutConfirmationReceipt>,
    created_unix_ns: u64,
    fitting_artifact: Option<CalibrationArtifact>,
    validation_report_sha256: Option<[u8; 32]>,
    terminal_failure: Option<TerminalFailure>,
}

impl HardwareCalibrationRunner {
    /// Open all selected devices, lock both wire identities, and bind the UI.
    pub fn start(config: HardwareCalibrationConfig) -> Result<Self, HardwareCalibrationError> {
        validate_paths(&config)?;
        let keyboard = LinuxKeyboardDevice::open(&config.keyboard_device)?;
        let keyboard_source = keyboard.identity();
        let initially_held_codes = keyboard.held_key_codes()?;
        let left = open_controller_source(&config.left_device)?.into_file();
        let right = open_controller_source(&config.right_device)?.into_file();
        let clock = HostTimeOrigin::new();
        let sensor_input = spawn_sensor_readers(left, right, Arc::clone(&config.stop), &clock);
        let (manifest, bootstrap_events) =
            bootstrap_manifest(config.template, &sensor_input, &clock, &config.stop)?;

        std::fs::create_dir(&config.output_directory)?;
        write_new(
            &config.output_directory.join("locked-manifest.json"),
            &manifest.canonical_json()?,
        )?;

        let keyboard_delivery = Arc::new(Mutex::new(()));
        let keyboard_input = spawn_keyboard_reader(
            keyboard,
            Arc::clone(&config.stop),
            &clock,
            Arc::clone(&keyboard_delivery),
        );
        let created_unix_ns = unix_time_ns()?;
        let mut worker = CalibrationCaptureWorker::new(CalibrationCaptureWorkerConfig {
            manifest: manifest.clone(),
            keyboard_source: keyboard_source.clone(),
            purpose: EvidencePurpose::Fitting,
            matching_rule: None,
            session_id: random_session_id()?,
            created_unix_ns,
            started_host_time_ns: 0,
            surface_plan: None,
            transport: DeviceTransportConfig::default(),
        })?;
        for event in bootstrap_events {
            worker.handle_sensor(event)?;
        }
        worker.handle_sensor(DeviceTransportEvent::Tick {
            host_time_ns: clock.now_ns(),
        })?;

        let app = CalibrationApp::new(EvidenceMode::Hardware);
        app.apply_hardware_event(HardwareCalibrationEvent::Connected)?;
        let server =
            CalibrationHttpServer::bind_with_host_clock(app.clone(), config.port, clock.clone())?;
        Ok(Self {
            app,
            server,
            stop: config.stop,
            clock,
            sensor_input,
            keyboard_input,
            keyboard_delivery,
            keyboard_source,
            selected_keyboard_state: SelectedKeyboardState::from_held_codes(initially_held_codes),
            manifest,
            output_directory: config.output_directory,
            worker: Some(worker),
            frozen_capture: None,
            stage: CaptureStage::HardwareCheck,
            capture_lease_revision: None,
            keyboard_layout_confirmation: None,
            created_unix_ns,
            fitting_artifact: None,
            validation_report_sha256: None,
            terminal_failure: None,
        })
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.server.local_addr()
    }

    /// Run one finite calibration attempt. Any lost source or browser lease is
    /// terminal so an interrupted attempt cannot be mistaken for a validated one.
    pub fn run(mut self) -> Result<HardwareCalibrationOutcome, HardwareCalibrationError> {
        let active_result = self
            .run_active()
            .and_then(|()| self.commit_validation_passed());
        match active_result {
            Ok(outcome) => Ok(outcome),
            Err(error) => {
                let terminal = self.terminal_failure.take().unwrap_or_else(|| {
                    TerminalFailure::new(
                        HardwareCalibrationTerminalCause::ProcessingFailure,
                        error.to_string(),
                    )
                });
                if let Err(commit_error) = self.commit_terminal(&terminal) {
                    return Err(invalid_data(&format!(
                        "{}; terminal commit failed: {commit_error}",
                        terminal.detail
                    )));
                }
                Err(error)
            }
        }
    }

    fn run_active(&mut self) -> Result<(), HardwareCalibrationError> {
        loop {
            if self.stop.load(Ordering::SeqCst) {
                return self.terminal_error(
                    HardwareCalibrationTerminalCause::Aborted,
                    "hardware calibration was stopped",
                );
            }
            let _ = self.server.poll_once()?;
            let control = self.app.control_state()?;
            if let Some(terminal) = self.detect_control_terminal(control) {
                if let Some(worker) = self.worker.as_mut() {
                    worker.close_label_admission();
                }
                let message = terminal.detail.clone();
                self.terminal_failure = Some(terminal);
                return Err(invalid_data(&message));
            }
            self.drain_keyboard()?;
            match self.sensor_input.recv_timeout(LOOP_WAIT) {
                Ok(input) => self.handle_sensor_input(input)?,
                Err(RecvTimeoutError::Timeout) => {
                    let now = self.clock.now_ns();
                    self.handle_worker_sensor(DeviceTransportEvent::Tick { host_time_ns: now })?
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return self.terminal_error(
                        HardwareCalibrationTerminalCause::SourceFault,
                        "both controller reader channels ended",
                    );
                }
            }
            self.drain_sensor_queue()?;
            self.advance()?;
            if matches!(self.stage, CaptureStage::Complete) {
                return Ok(());
            }
        }
    }

    fn detect_control_terminal(&self, control: CalibrationControlState) -> Option<TerminalFailure> {
        let CalibrationControlState {
            phase,
            failure,
            capture_lease_revision,
            paused,
        } = control;
        if phase == CalibrationPhase::Aborted {
            return Some(TerminalFailure::new(
                HardwareCalibrationTerminalCause::Aborted,
                "hardware calibration was aborted",
            ));
        }
        if matches!(
            phase,
            CalibrationPhase::CalibrationFailed | CalibrationPhase::NeedsHardwareAttention
        ) {
            return Some(match failure {
                Some(CalibrationFailure::HardwareFault) => TerminalFailure::new(
                    HardwareCalibrationTerminalCause::ProcessingFailure,
                    "sensor stream or timing became invalid",
                ),
                Some(CalibrationFailure::UnsupportedLayout) => TerminalFailure::new(
                    HardwareCalibrationTerminalCause::UnsupportedLayout,
                    "selected keyboard layout is unsupported by the Core-28 on a 34-position physical grid setup",
                ),
                Some(CalibrationFailure::SurfaceValidation) => TerminalFailure::new(
                    HardwareCalibrationTerminalCause::ProcessingFailure,
                    "held-out validation failed its fixed key and no-key checks",
                ),
                Some(CalibrationFailure::ValidationPlateau) => TerminalFailure::new(
                    HardwareCalibrationTerminalCause::ProcessingFailure,
                    "keyboard signal validation reached the fixed plateau rule",
                ),
                Some(CalibrationFailure::ExampleCap) => TerminalFailure::new(
                    HardwareCalibrationTerminalCause::ProcessingFailure,
                    "calibration reached the per-position evidence cap",
                ),
                Some(CalibrationFailure::TimeCap) => TerminalFailure::new(
                    HardwareCalibrationTerminalCause::TimeCap,
                    "setup exceeded the finite 20-minute cap",
                ),
                None => TerminalFailure::new(
                    HardwareCalibrationTerminalCause::ProcessingFailure,
                    "calibration failed without a structured cause",
                ),
            });
        }
        if paused
            || self
                .capture_lease_revision
                .is_some_and(|expected| capture_lease_revision != Some(expected))
        {
            return Some(TerminalFailure::new(
                HardwareCalibrationTerminalCause::CaptureLeaseLost,
                "browser capture lease ended; restart the setup command",
            ));
        }
        None
    }

    fn handle_sensor_input(&mut self, input: SensorInput) -> Result<(), HardwareCalibrationError> {
        let event = match input {
            SensorInput::Chunk {
                source,
                receipt_time_ns,
                bytes,
            } => DeviceTransportEvent::Bytes {
                source,
                receipt_time_ns,
                bytes,
            },
            SensorInput::End {
                source,
                receipt_time_ns,
            } => DeviceTransportEvent::Disconnected {
                source,
                receipt_time_ns,
            },
            SensorInput::Failed {
                source,
                receipt_time_ns,
                error,
            } => {
                return self.terminal_error(
                    HardwareCalibrationTerminalCause::SourceFault,
                    format!("{source:?} controller read failed at {receipt_time_ns} ns: {error}"),
                );
            }
        };
        self.handle_worker_sensor(event)
    }

    /// Dispatch one finite batch; return whether the entire batch was consumed.
    fn drain_keyboard(&mut self) -> Result<bool, HardwareCalibrationError> {
        // Bound dispatch work, not just storage: the reader can refill the
        // queue while we handle events.
        for _ in 0..DEVICE_CHANNEL_CAPACITY {
            match self.keyboard_input.try_recv() {
                Ok(KeyboardInput::Event(event)) => self.handle_keyboard_event(event)?,
                Ok(KeyboardInput::Failed(error)) => {
                    return self.terminal_error(
                        HardwareCalibrationTerminalCause::SourceFault,
                        format!("selected keyboard read failed: {error}"),
                    );
                }
                Err(TryRecvError::Empty) => return Ok(false),
                Err(TryRecvError::Disconnected) => {
                    return self.terminal_error(
                        HardwareCalibrationTerminalCause::SourceFault,
                        "selected keyboard reader ended",
                    );
                }
            }
        }
        Ok(true)
    }

    fn drain_sensor_queue(&mut self) -> Result<(), HardwareCalibrationError> {
        for _ in 0..DEVICE_CHANNEL_CAPACITY {
            match self.sensor_input.try_recv() {
                Ok(input) => self.handle_sensor_input(input)?,
                Err(TryRecvError::Empty) => return Ok(()),
                Err(TryRecvError::Disconnected) => {
                    return self.terminal_error(
                        HardwareCalibrationTerminalCause::SourceFault,
                        "both controller reader channels ended",
                    );
                }
            }
        }
        Ok(())
    }

    fn handle_keyboard_event(
        &mut self,
        event: RawKeyboardEvent,
    ) -> Result<(), HardwareCalibrationError> {
        // Observe before stage-specific handling so even a rejected event keeps
        // worker-boundary key state fail closed.
        self.selected_keyboard_state.observe(&event);
        let source = self.keyboard_source.clone();
        let stage = std::mem::replace(&mut self.stage, CaptureStage::Complete);
        self.stage = match stage {
            CaptureStage::KeyboardIdentification => {
                self.worker_mut()?.handle_keyboard(
                    &source,
                    event,
                    KeyboardCaptureContext::new(
                        EvidenceRole::Training,
                        EvidenceDomain::Keyboard,
                        10,
                    ),
                )?;
                if self.worker_mut()?.keyboard_identified() {
                    self.app.apply_hardware_event(
                        HardwareCalibrationEvent::KeyboardDeviceIdentified {
                            device: format!("{} ({})", source.name, source.device_id),
                        },
                    )?;
                    CaptureStage::AwaitLayoutDecision
                } else {
                    CaptureStage::KeyboardIdentification
                }
            }
            CaptureStage::AwaitLayoutDecision => {
                self.worker_mut()?.handle_keyboard(
                    &source,
                    event,
                    KeyboardCaptureContext::new(
                        EvidenceRole::Training,
                        EvidenceDomain::Keyboard,
                        9,
                    )
                    .state_only(),
                )?;
                CaptureStage::AwaitLayoutDecision
            }
            CaptureStage::Neutral {
                started_ns,
                start_record,
                displayed_seconds,
            } => {
                self.worker_mut()?.handle_keyboard(
                    &source,
                    event,
                    KeyboardCaptureContext::new(
                        EvidenceRole::Training,
                        EvidenceDomain::Keyboard,
                        9,
                    )
                    .state_only(),
                )?;
                CaptureStage::Neutral {
                    started_ns,
                    start_record,
                    displayed_seconds,
                }
            }
            CaptureStage::KeyboardTraining(mut training) => {
                let block = keyboard_block(
                    event.physical_code,
                    event.state,
                    training.prompt_attempt,
                    &mut training.blocks_by_code,
                );
                let candidate_attempt = training.candidate_attempt(&event);
                let context = KeyboardCaptureContext::new(
                    EvidenceRole::Training,
                    EvidenceDomain::Keyboard,
                    block,
                );
                let disposition = self.worker_mut()?.handle_keyboard(
                    &source,
                    event,
                    if candidate_attempt {
                        context
                    } else {
                        context.state_only()
                    },
                )?;
                if let LabelDisposition::Accepted(label) = disposition
                    && label.state == RawKeyState::Down
                    && training.prompt.current() == Some(label.position)
                {
                    let index = position_index(label.position).expect("accepted position is known");
                    if candidate_attempt {
                        training.raw_attempts[index] =
                            training.raw_attempts[index].saturating_add(1);
                    }
                    if training.prompt.accept(label.position) {
                        if training.prompt.is_complete() {
                            training.drain_after_ns =
                                Some(label.host_time_ns.saturating_add(PLAN_DRAIN_NS));
                        }
                        self.publish_keyboard_prompt(&training.prompt)?;
                    }
                }
                CaptureStage::KeyboardTraining(training)
            }
            CaptureStage::SurfaceTraining(timed) => {
                self.worker_mut()?.handle_keyboard(
                    &source,
                    event,
                    KeyboardCaptureContext::new(
                        EvidenceRole::Training,
                        EvidenceDomain::Keyboard,
                        30,
                    )
                    .state_only(),
                )?;
                CaptureStage::SurfaceTraining(timed)
            }
            CaptureStage::HeldOutValidation(timed) => {
                self.worker_mut()?.handle_keyboard(
                    &source,
                    event,
                    KeyboardCaptureContext::new(
                        EvidenceRole::HeldOutValidation,
                        EvidenceDomain::Keyboard,
                        40,
                    )
                    .state_only(),
                )?;
                CaptureStage::HeldOutValidation(timed)
            }
            other => other,
        };
        Ok(())
    }

    fn advance(&mut self) -> Result<(), HardwareCalibrationError> {
        let now = self.clock.now_ns();
        let elapsed_seconds = u32::try_from(now / 1_000_000_000).unwrap_or(u32::MAX);
        self.app.observe_elapsed_seconds(elapsed_seconds)?;
        if elapsed_seconds >= crate::calibration::SETUP_TIME_CAP_SECONDS {
            return self.terminal_error(
                HardwareCalibrationTerminalCause::TimeCap,
                "setup exceeded the finite 20-minute cap",
            );
        }
        let stage = std::mem::replace(&mut self.stage, CaptureStage::Complete);
        self.stage = match stage {
            CaptureStage::HardwareCheck => {
                let healthy = {
                    let required_frames = health_check_frames(self.manifest.nominal_period_us);
                    let worker = self.worker_mut()?;
                    [Hand::Left, Hand::Right]
                        .into_iter()
                        .all(|hand| worker.valid_frame_count(hand) >= required_frames)
                };
                if healthy {
                    self.app
                        .apply_hardware_event(HardwareCalibrationEvent::HardwareChecked)?;
                    CaptureStage::KeyboardIdentification
                } else {
                    CaptureStage::HardwareCheck
                }
            }
            CaptureStage::AwaitLayoutDecision => self.advance_layout_decision(now)?,
            CaptureStage::Neutral {
                mut started_ns,
                mut start_record,
                mut displayed_seconds,
            } => {
                self.require_live_lease()?;
                let elapsed = now.saturating_sub(started_ns);
                let remaining =
                    3_u8.saturating_sub(u8::try_from(elapsed / 1_000_000_000).unwrap_or(u8::MAX));
                if remaining != displayed_seconds {
                    displayed_seconds = remaining;
                    self.publish_instruction(None, None, Some(remaining))?;
                }
                if elapsed >= NEUTRAL_HOLD_NS {
                    self.handle_worker_sensor(DeviceTransportEvent::Tick { host_time_ns: now })?;
                    let window = NeutralRecordWindow {
                        block_id: "neutral-1".to_owned(),
                        window_id: "comfortable-home-pose".to_owned(),
                        role: NeutralWindowRole::Neutral,
                        start_record,
                        end_record_exclusive: self.worker_mut()?.record_count(),
                    };
                    let report = self
                        .worker_mut()?
                        .derive_neutral_report(std::slice::from_ref(&window));
                    let report = match report {
                        Ok(report) => report,
                        Err(error) => {
                            return self.fail(&format!(
                                "neutral sensor evidence failed integrity checks: {error}"
                            ));
                        }
                    };
                    if report.verdict == StabilityVerdict::Stable {
                        self.worker_mut()?.set_neutral_windows(vec![window])?;
                        self.app
                            .apply_hardware_event(HardwareCalibrationEvent::NeutralAccepted)?;
                        let training = KeyboardTrainingState::new();
                        self.publish_keyboard_prompt(&training.prompt)?;
                        CaptureStage::KeyboardTraining(training)
                    } else {
                        started_ns = now;
                        start_record = self.worker_mut()?.record_count();
                        displayed_seconds = 3;
                        self.publish_instruction(
                            Some(
                                "Movement was detected. Rest both hands and hold still.".to_owned(),
                            ),
                            None,
                            Some(3),
                        )?;
                        CaptureStage::Neutral {
                            started_ns,
                            start_record,
                            displayed_seconds,
                        }
                    }
                } else {
                    CaptureStage::Neutral {
                        started_ns,
                        start_record,
                        displayed_seconds,
                    }
                }
            }
            CaptureStage::KeyboardTraining(mut training) => {
                self.require_live_lease()?;
                let Some(drain_after_ns) = training.drain_after_ns else {
                    self.stage = CaptureStage::KeyboardTraining(training);
                    return Ok(());
                };
                let sensor_horizon = self.worker_mut()?.sensor_receipt_horizon_ns();
                if now < drain_after_ns
                    || sensor_horizon.is_none_or(|horizon| horizon < drain_after_ns)
                {
                    CaptureStage::KeyboardTraining(training)
                } else {
                    self.handle_worker_sensor(DeviceTransportEvent::Tick { host_time_ns: now })?;
                    training.evidence = match self.worker_mut()?.keyboard_evidence_checkpoint() {
                        Ok(evidence) => evidence,
                        Err(CalibrationWorkerError::Capture(
                            CalibrationCaptureError::StreamIntegrity,
                        )) => {
                            return self.fail(
                                "keyboard calibration sensor stream failed integrity checks",
                            );
                        }
                        Err(error) => {
                            return self.fail(&format!(
                                "keyboard calibration evidence checkpoint failed: {error}"
                            ));
                        }
                    };
                    if training
                        .evidence
                        .is_complete(KEYBOARD_EXAMPLES_PER_POSITION)
                    {
                        if !self.keyboard_boundary_is_neutral()? {
                            self.stage = CaptureStage::KeyboardTraining(training);
                            return Ok(());
                        }
                        self.publish_matched_keyboard_coverage(&training.evidence)?;
                        self.finish_keyboard_checkpoint(now)?;
                        return Ok(());
                    }
                    if let Some(position) = training.capped_deficit() {
                        return self.fail(&format!(
                            "keyboard calibration reached the {}-attempt cap for {} without enough synchronized sensor evidence",
                            MAX_EXAMPLES_PER_POSITION,
                            label_for(position),
                        ));
                    }
                    training.prompt_attempt = training.prompt_attempt.saturating_add(1);
                    training.prompt =
                        keyboard_prompt_state(&training.evidence, training.prompt_attempt);
                    training.drain_after_ns = None;
                    self.publish_keyboard_prompt(&training.prompt)?;
                    CaptureStage::KeyboardTraining(training)
                }
            }
            CaptureStage::SurfaceTraining(timed) => {
                self.stage = CaptureStage::SurfaceTraining(timed);
                self.advance_surface_training(now)?;
                return Ok(());
            }
            CaptureStage::ArtifactFitting { result } => {
                self.stage = CaptureStage::ArtifactFitting { result };
                self.advance_artifact_fitting(now)?;
                return Ok(());
            }
            CaptureStage::HeldOutValidation(timed) => {
                self.stage = CaptureStage::HeldOutValidation(timed);
                self.advance_heldout(now)?;
                return Ok(());
            }
            CaptureStage::HeldOutValidating { result } => {
                self.stage = CaptureStage::HeldOutValidating { result };
                self.advance_heldout_validation()?;
                return Ok(());
            }
            CaptureStage::KeyboardIdentification => CaptureStage::KeyboardIdentification,
            CaptureStage::Complete => CaptureStage::Complete,
        };
        Ok(())
    }

    fn advance_layout_decision(
        &mut self,
        now: u64,
    ) -> Result<CaptureStage, HardwareCalibrationError> {
        let receipt = match self.app.keyboard_layout_decision_state()? {
            KeyboardLayoutDecisionState::WaitingForDevice
            | KeyboardLayoutDecisionState::Pending { .. } => {
                return Ok(CaptureStage::AwaitLayoutDecision);
            }
            KeyboardLayoutDecisionState::Rejected { .. } => {
                return self.terminal_error(
                    HardwareCalibrationTerminalCause::UnsupportedLayout,
                    "selected keyboard layout is unsupported by Core-28 setup",
                );
            }
            KeyboardLayoutDecisionState::Confirmed(receipt) => receipt,
        };
        if !self.keyboard_boundary_is_neutral()? {
            return Ok(CaptureStage::AwaitLayoutDecision);
        }
        if self.keyboard_layout_confirmation.is_none() {
            write_new(
                &self.output_directory.join("keyboard-layout-decision.json"),
                &serde_json::to_vec(&receipt)?,
            )?;
            self.worker_mut()?
                .confirm_keyboard_layout(receipt.clone())?;
            self.keyboard_layout_confirmation = Some(receipt.clone());
        }
        let snapshot = self
            .app
            .activate_confirmed_layout_capture(receipt.instruction_revision)?;
        let lease = snapshot
            .capture_lease_revision
            .ok_or_else(|| invalid_data("confirmed layout did not arm a capture lease"))?;
        self.capture_lease_revision = Some(lease);
        self.publish_instruction(None, None, Some(3))?;
        Ok(CaptureStage::Neutral {
            started_ns: now,
            start_record: self.worker_mut()?.record_count(),
            displayed_seconds: 3,
        })
    }

    fn finish_keyboard_checkpoint(&mut self, now: u64) -> Result<(), HardwareCalibrationError> {
        self.handle_worker_sensor(DeviceTransportEvent::Tick { host_time_ns: now })?;
        let bundle = match self.worker_mut()?.checkpoint() {
            Ok(bundle) => bundle,
            Err(error) => {
                return self.fail(&format!(
                    "keyboard calibration checkpoint failed integrity checks: {error}"
                ));
            }
        };
        self.worker_mut()?
            .freeze_contact_rule(bundle.capture_report.matching_rule.clone())?;
        let precheck = evaluate_keyboard_precheck(
            &self.manifest,
            &bundle.clocks,
            &bundle.journal,
            &bundle.session,
        )?;
        persist_bundle(&self.output_directory, "keyboard-checkpoint", &bundle)?;
        replay_keyboard_checkpoint(&self.app, &bundle, &precheck)?;

        let plan = SurfaceTrainingPlan::v1();
        let timed = timed_training_plan(
            &plan,
            now.saturating_add(PLAN_LEAD_IN_NS),
            self.manifest.nominal_period_us,
        );
        self.worker_mut()?
            .bind_surface_plan(CalibrationSurfacePlanBinding::fitting(timed.started_ns)?)?;
        write_new(
            &self.output_directory.join("surface-training-plan.json"),
            &timed.canonical_plan,
        )?;
        self.stage = CaptureStage::SurfaceTraining(timed);
        self.publish_instruction(
            None,
            Some(
                "Move the regular keyboard away. The first target starts in two seconds."
                    .to_owned(),
            ),
            None,
        )?;
        Ok(())
    }

    fn advance_surface_training(&mut self, now: u64) -> Result<(), HardwareCalibrationError> {
        self.require_live_lease()?;
        let mut timed = take_timed_stage(&mut self.stage, true)?;
        self.process_timed(&mut timed, now)?;
        if !timed_capture_drained(&timed, now) {
            self.stage = CaptureStage::SurfaceTraining(timed);
            return Ok(());
        }
        self.stage = CaptureStage::SurfaceTraining(timed);
        let Some(transitioned) = self.try_keyboard_delivery_boundary(|runner| {
            let timed = take_timed_stage(&mut runner.stage, true)?;
            if !runner.keyboard_boundary_is_neutral()? {
                runner.stage = CaptureStage::SurfaceTraining(timed);
                return Ok(false);
            }

            let fitting_worker = runner.freeze_capture(now, "fitting", timed.display_receipts)?;
            let result = spawn_artifact_fit(runner.manifest.clone(), fitting_worker);
            runner.stage = CaptureStage::ArtifactFitting { result };
            Ok(true)
        })?
        else {
            return Ok(());
        };
        if !transitioned {
            return Ok(());
        }
        self.publish_instruction(
            None,
            Some("Fitting the locked personal model. Keep this page visible.".to_owned()),
            None,
        )?;
        Ok(())
    }

    fn advance_artifact_fitting(&mut self, now: u64) -> Result<(), HardwareCalibrationError> {
        self.require_live_lease()?;
        if !self.keyboard_boundary_is_neutral()? {
            return Ok(());
        }
        let stage = std::mem::replace(&mut self.stage, CaptureStage::Complete);
        let CaptureStage::ArtifactFitting { result } = stage else {
            self.stage = stage;
            return Err(invalid_data("artifact-fitting stage changed unexpectedly"));
        };
        let fitted = match result.try_recv() {
            Ok(Ok(fitted)) => fitted,
            Ok(Err(error)) => return Err(error),
            Err(TryRecvError::Empty) => {
                self.stage = CaptureStage::ArtifactFitting { result };
                return Ok(());
            }
            Err(TryRecvError::Disconnected) => {
                return self.fail("artifact-fitting worker ended without a result");
            }
        };

        persist_bundle(&self.output_directory, "fitting", &fitted.bundle)?;
        self.persist_frozen_display_receipts()?;
        write_new(
            &self.output_directory.join("calibration-artifact.json"),
            &fitted.artifact.canonical_json()?,
        )?;
        replay_surface_fit(&self.app, self.lease()?, &fitted.bundle, &fitted.artifact)?;
        let plan = HeldOutSurfacePlan::v1(fitted.artifact.fingerprint()?, random_u64()?);
        let timed = timed_heldout_plan(
            &plan,
            now.saturating_add(PLAN_LEAD_IN_NS),
            self.manifest.nominal_period_us,
        );
        write_new(
            &self.output_directory.join("heldout-surface-plan.json"),
            &timed.canonical_plan,
        )?;
        let mut heldout_worker = CalibrationCaptureWorker::new(CalibrationCaptureWorkerConfig {
            manifest: self.manifest.clone(),
            keyboard_source: self.keyboard_source.clone(),
            purpose: EvidencePurpose::HeldOutValidation,
            matching_rule: Some(fitted.bundle.capture_report.matching_rule.clone()),
            session_id: random_session_id()?,
            created_unix_ns: self.created_unix_ns,
            started_host_time_ns: now,
            surface_plan: Some(CalibrationSurfacePlanBinding::held_out(
                &plan,
                timed.started_ns,
            )?),
            transport: DeviceTransportConfig::default(),
        })?;
        heldout_worker.confirm_keyboard_layout(
            self.keyboard_layout_confirmation
                .clone()
                .ok_or_else(|| invalid_data("held-out capture has no layout receipt"))?,
        )?;
        self.worker_mut()?
            .transfer_transport_to(&mut heldout_worker)?;
        self.worker = Some(heldout_worker);
        self.frozen_capture = None;
        self.fitting_artifact = Some(fitted.artifact);
        self.stage = CaptureStage::HeldOutValidation(timed);
        self.publish_instruction(
            None,
            Some(
                "Held-out validation starts in two seconds. Do not put the keyboard back."
                    .to_owned(),
            ),
            None,
        )?;
        Ok(())
    }

    fn advance_heldout(&mut self, now: u64) -> Result<(), HardwareCalibrationError> {
        self.require_live_lease()?;
        let mut timed = take_timed_stage(&mut self.stage, false)?;
        self.process_timed(&mut timed, now)?;
        if !timed_capture_drained(&timed, now) {
            self.stage = CaptureStage::HeldOutValidation(timed);
            return Ok(());
        }
        self.stage = CaptureStage::HeldOutValidation(timed);
        let Some(transitioned) = self.try_keyboard_delivery_boundary(|runner| {
            let timed = take_timed_stage(&mut runner.stage, false)?;
            if !runner.keyboard_boundary_is_neutral()? {
                runner.stage = CaptureStage::HeldOutValidation(timed);
                return Ok(false);
            }

            let artifact = runner
                .fitting_artifact
                .as_ref()
                .ok_or_else(|| invalid_data("missing locked artifact"))?
                .clone();
            let heldout_worker = runner.freeze_capture(now, "heldout", timed.display_receipts)?;
            let result =
                spawn_heldout_validation(runner.manifest.clone(), artifact, heldout_worker);
            runner.stage = CaptureStage::HeldOutValidating { result };
            Ok(true)
        })?
        else {
            return Ok(());
        };
        if !transitioned {
            return Ok(());
        }
        self.publish_instruction(
            None,
            Some("Checking the locked artifact. Keep this page visible.".to_owned()),
            None,
        )?;
        Ok(())
    }

    fn advance_heldout_validation(&mut self) -> Result<(), HardwareCalibrationError> {
        self.require_live_lease()?;
        let stage = std::mem::replace(&mut self.stage, CaptureStage::Complete);
        let CaptureStage::HeldOutValidating { result } = stage else {
            self.stage = stage;
            return Err(invalid_data(
                "held-out validation stage changed unexpectedly",
            ));
        };
        let validated = match result.try_recv() {
            Ok(Ok(validated)) => validated,
            Ok(Err(error)) => return Err(error),
            Err(TryRecvError::Empty) => {
                self.stage = CaptureStage::HeldOutValidating { result };
                return Ok(());
            }
            Err(TryRecvError::Disconnected) => {
                return self.fail("held-out validation worker ended without a result");
            }
        };

        persist_bundle(&self.output_directory, "heldout", &validated.bundle)?;
        self.persist_frozen_display_receipts()?;
        let report_sha256 = validated.validation.report.fingerprint()?;
        write_new(
            &self.output_directory.join("validation-report.json"),
            &validated.validation.report.canonical_json()?,
        )?;
        self.app
            .apply_hardware_event(HardwareCalibrationEvent::SurfaceValidation(
                validated.validation.report,
            ))?;
        self.capture_lease_revision = None;
        if let Some(worker) = self.worker.as_mut() {
            worker.close_label_admission();
        }
        self.validation_report_sha256 = Some(report_sha256);
        self.frozen_capture = None;
        self.stage = CaptureStage::Complete;
        Ok(())
    }

    fn process_timed(
        &mut self,
        timed: &mut TimedSurfaceCapture,
        now: u64,
    ) -> Result<(), HardwareCalibrationError> {
        let elapsed = now.saturating_sub(timed.started_ns);
        loop {
            let Some(action) = timed.displays.get(timed.next_display) else {
                break;
            };
            let scheduled_ns = action
                .at_ns
                .checked_add(timed.schedule_shift_ns)
                .ok_or_else(|| invalid_data("surface display schedule exceeded the host clock"))?;
            if elapsed < scheduled_ns {
                break;
            }
            if action.pending.is_none() {
                let snapshot = self.app.apply_hardware_event(
                    HardwareCalibrationEvent::CaptureInstruction {
                        keyboard_prompt: None,
                        surface_target: Some(action.instruction.clone()),
                        surface_position: action.highlighted_position,
                        surface_instruction_receipt_required: true,
                        neutral_seconds: None,
                        capture_lease_revision: self.lease()?,
                    },
                )?;
                let revision = snapshot.surface_instruction_revision.ok_or_else(|| {
                    invalid_data("surface instruction did not create a pending render receipt")
                })?;
                timed.displays[timed.next_display].pending = Some(PendingSurfaceInstruction {
                    revision,
                    published_at_ns: now,
                });
                break;
            }

            let pending = timed.displays[timed.next_display]
                .pending
                .as_ref()
                .expect("checked pending instruction");
            let Some(receipt) = self.app.take_surface_instruction_ack(pending.revision)? else {
                break;
            };
            let planned_at_ns = timed.displays[timed.next_display].at_ns;
            let published_at_ns = pending.published_at_ns;
            let instruction_revision = pending.revision;
            if receipt.instruction_revision != instruction_revision {
                return Err(invalid_data(
                    "surface instruction receipt revision changed before capture",
                ));
            }
            let acknowledged_at_ns = receipt.acknowledged_host_time_ns;
            let prompted_position = timed.displays[timed.next_display].prompted_position;
            let prompt_id = timed.displays[timed.next_display].prompt_id.clone();
            let phase = timed.displays[timed.next_display].phase;
            let planned_host_time_ns = timed
                .started_ns
                .checked_add(planned_at_ns)
                .and_then(|time| time.checked_add(timed.schedule_shift_ns))
                .ok_or_else(|| invalid_data("surface display schedule exceeded the host clock"))?;
            self.worker_mut()?.record_display_acknowledgement(
                prompt_id.clone(),
                SurfacePresentationReceipt::new(
                    planned_host_time_ns,
                    published_at_ns,
                    acknowledged_at_ns,
                    instruction_revision,
                )?,
            )?;
            let delay_ns = acknowledged_at_ns
                .checked_sub(planned_host_time_ns)
                .ok_or_else(|| invalid_data("surface display acknowledgement preceded its plan"))?;
            timed.schedule_shift_ns = timed
                .schedule_shift_ns
                .checked_add(delay_ns)
                .ok_or_else(|| invalid_data("surface display delay exceeded the host clock"))?;
            timed.display_receipts.push(SurfaceDisplayReceipt {
                prompt_id,
                phase,
                planned_offset_ns: planned_at_ns,
                published_at_ns,
                acknowledged_at_ns,
                instruction_revision,
                prompted_position,
            });
            timed.displays[timed.next_display].pending = None;
            timed.next_display += 1;
        }
        // A target or null interval is evidence only after the browser proves
        // that the current cue reached its rendered DOM. Do not let a slow or
        // backgrounded page label sensor activity against an unshown plan.
        if timed
            .displays
            .get(timed.next_display)
            .is_some_and(|display| display.pending.is_some())
        {
            return Ok(());
        }
        while timed.nulls.get(timed.next_null).is_some_and(|action| {
            action
                .at_ns
                .checked_add(timed.schedule_shift_ns)
                .is_some_and(|scheduled_ns| elapsed >= scheduled_ns)
        }) {
            let action = &timed.nulls[timed.next_null];
            let trigger_host_time_ns = timed
                .started_ns
                .checked_add(action.at_ns)
                .and_then(|time| time.checked_add(timed.schedule_shift_ns))
                .ok_or_else(|| invalid_data("surface null schedule exceeded the host clock"))?;
            self.worker_mut()?.record_idle(
                action.prompt_id.clone(),
                action.sample_ordinal,
                trigger_host_time_ns,
            )?;
            timed.next_null += 1;
        }
        Ok(())
    }

    fn publish_keyboard_prompt(
        &self,
        prompt: &KeyboardPromptState,
    ) -> Result<(), HardwareCalibrationError> {
        self.publish_instruction(Some(prompt.rendered()), None, None)
    }

    fn publish_matched_keyboard_coverage(
        &self,
        evidence: &KeyboardEvidenceCheckpoint,
    ) -> Result<(), HardwareCalibrationError> {
        let lease = self.lease()?;
        for observation in accepted_keyboard_observations(evidence) {
            self.app
                .apply_hardware_event(HardwareCalibrationEvent::ExampleAccepted {
                    domain: CalibrationDomain::Keyboard,
                    position: observation.position,
                    block: BlockId::new(observation.block_id),
                    capture_lease_revision: lease,
                })?;
        }
        Ok(())
    }

    fn publish_instruction(
        &self,
        keyboard_prompt: Option<String>,
        surface_target: Option<String>,
        neutral_seconds: Option<u8>,
    ) -> Result<(), HardwareCalibrationError> {
        self.app
            .apply_hardware_event(HardwareCalibrationEvent::CaptureInstruction {
                keyboard_prompt,
                surface_target,
                surface_position: None,
                surface_instruction_receipt_required: false,
                neutral_seconds,
                capture_lease_revision: self.lease()?,
            })?;
        Ok(())
    }

    fn require_live_lease(&mut self) -> Result<(), HardwareCalibrationError> {
        if let Some(terminal) = &self.terminal_failure {
            return Err(invalid_data(&terminal.detail));
        }
        if let Err(error) = self.worker_mut()?.require_healthy() {
            return self.terminal_error(
                HardwareCalibrationTerminalCause::StreamCorruption,
                format!("controller sensor stream became unusable: {error}"),
            );
        }
        let Some(lease_revision) = self.capture_lease_revision else {
            return self.terminal_error(
                HardwareCalibrationTerminalCause::CaptureLeaseLost,
                "browser capture lease ended; restart the setup command",
            );
        };
        if !self.app.capture_lease_is_live(lease_revision)? {
            return self.terminal_error(
                HardwareCalibrationTerminalCause::CaptureLeaseLost,
                "browser capture lease ended; restart the setup command",
            );
        }
        Ok(())
    }

    fn lease(&self) -> Result<u64, HardwareCalibrationError> {
        self.capture_lease_revision
            .ok_or_else(|| invalid_data("capture lease is not active"))
    }

    fn worker_mut(&mut self) -> Result<&mut CalibrationCaptureWorker, HardwareCalibrationError> {
        self.worker
            .as_mut()
            .ok_or_else(|| invalid_data("capture worker is not active"))
    }

    fn keyboard_boundary_is_neutral(&mut self) -> Result<bool, HardwareCalibrationError> {
        Ok(self.selected_keyboard_state.is_neutral() && self.worker_mut()?.held_key_count() == 0)
    }

    /// Attempt one worker transition while keyboard delivery is quiescent.
    ///
    /// The reader assigns host ownership while holding this same gate. Draining
    /// after acquiring it therefore puts every pre-transition event in the old
    /// worker, while any later event receives a post-transition host time.
    fn try_keyboard_delivery_boundary<T>(
        &mut self,
        transition: impl FnOnce(&mut Self) -> Result<T, HardwareCalibrationError>,
    ) -> Result<Option<T>, HardwareCalibrationError> {
        let delivery = Arc::clone(&self.keyboard_delivery);
        match delivery.try_lock() {
            Ok(_guard) => {
                // Only this quiescent path may keep draining to empty. The
                // successful-event producer cannot replenish the bounded queue.
                while self.drain_keyboard()? {}
                transition(self).map(Some)
            }
            Err(TryLockError::WouldBlock) => {
                // A sender can hold the gate while a bounded channel is full.
                // Drain one finite batch, then retry through the normal run loop.
                self.drain_keyboard()?;
                Ok(None)
            }
            Err(TryLockError::Poisoned(_)) => {
                Err(invalid_data("keyboard delivery boundary was poisoned"))
            }
        }
    }

    fn handle_worker_sensor(
        &mut self,
        event: DeviceTransportEvent,
    ) -> Result<(), HardwareCalibrationError> {
        if let Err(error) = self.worker_mut()?.handle_sensor(event) {
            return self.terminal_error(
                HardwareCalibrationTerminalCause::StreamCorruption,
                format!("controller sensor stream became unusable: {error}"),
            );
        }
        Ok(())
    }

    fn freeze_capture(
        &mut self,
        finished_host_time_ns: u64,
        prefix: &'static str,
        display_receipts: Vec<SurfaceDisplayReceipt>,
    ) -> Result<Arc<CalibrationCaptureWorker>, HardwareCalibrationError> {
        let mut monitor = self.new_monitor_worker(finished_host_time_ns)?;
        self.worker_mut()?.transfer_transport_to(&mut monitor)?;
        let mut worker = self
            .worker
            .take()
            .ok_or_else(|| invalid_data("missing capture worker to freeze"))?;
        let closed = worker.freeze(finished_host_time_ns);
        let worker = Arc::new(worker);
        self.frozen_capture = Some(FrozenCapture {
            worker: Arc::clone(&worker),
            prefix,
            display_receipts,
            display_receipts_persisted: false,
        });
        self.worker = Some(monitor);
        closed?;
        Ok(worker)
    }

    fn persist_frozen_display_receipts(&mut self) -> Result<(), HardwareCalibrationError> {
        let capture = self
            .frozen_capture
            .as_mut()
            .ok_or_else(|| invalid_data("missing frozen capture display receipts"))?;
        persist_display_receipts(
            &self.output_directory,
            capture.prefix,
            &capture.display_receipts,
        )?;
        capture.display_receipts_persisted = true;
        Ok(())
    }

    /// Drain and validate both live sources while a closed evidence bundle is
    /// processed off-thread. This worker is deliberately never finalized, so
    /// its unlabeled interval cannot enter fitting or held-out evidence.
    fn new_monitor_worker(
        &self,
        started_host_time_ns: u64,
    ) -> Result<CalibrationCaptureWorker, HardwareCalibrationError> {
        Ok(CalibrationCaptureWorker::new(
            CalibrationCaptureWorkerConfig {
                manifest: self.manifest.clone(),
                keyboard_source: self.keyboard_source.clone(),
                purpose: EvidencePurpose::Fitting,
                matching_rule: None,
                session_id: random_session_id()?,
                created_unix_ns: self.created_unix_ns,
                started_host_time_ns,
                surface_plan: None,
                transport: DeviceTransportConfig::default(),
            },
        )?)
    }

    fn fail<T>(&mut self, message: &str) -> Result<T, HardwareCalibrationError> {
        self.terminal_error(HardwareCalibrationTerminalCause::ProcessingFailure, message)
    }

    fn terminal_error<T>(
        &mut self,
        cause: HardwareCalibrationTerminalCause,
        detail: impl Into<String>,
    ) -> Result<T, HardwareCalibrationError> {
        let detail = detail.into();
        if self.terminal_failure.is_none() {
            self.terminal_failure = Some(TerminalFailure::new(cause, detail.clone()));
        }
        if let Some(worker) = self.worker.as_mut() {
            worker.close_label_admission();
        }
        Err(invalid_data(&detail))
    }

    fn commit_validation_passed(
        &mut self,
    ) -> Result<HardwareCalibrationOutcome, HardwareCalibrationError> {
        let control = self.app.control_state()?;
        if let Some(terminal) = self.detect_control_terminal(control) {
            return self.terminal_error(terminal.cause, terminal.detail);
        }
        if control.phase != CalibrationPhase::ValidationPassed {
            return self.terminal_error(
                HardwareCalibrationTerminalCause::ProcessingFailure,
                "held-out validation did not pass",
            );
        }
        let artifact = self
            .fitting_artifact
            .as_ref()
            .ok_or_else(|| invalid_data("completed setup has no artifact"))?;
        let artifact_sha256 = artifact.fingerprint()?;
        let validation_report_sha256 = self
            .validation_report_sha256
            .ok_or_else(|| invalid_data("completed setup has no validation report identity"))?;
        let published = self.app.publish_saved_result()?;
        let instruction_revision = published
            .result_acknowledgement_revision
            .ok_or_else(|| invalid_data("saved result has no acknowledgement revision"))?;
        let result_acknowledgement = self.await_result_acknowledgement(instruction_revision)?;
        let final_state = HardwareCalibrationFinalState {
            schema: HARDWARE_CALIBRATION_FINAL_STATE_SCHEMA.to_owned(),
            heldout_validation_passed: true,
            completed_unix_ns: unix_time_ns()?,
            disposition: HardwareCalibrationFinalDisposition::ValidationPassed {
                artifact_sha256: format_lower_hex(&artifact_sha256),
                validation_report_sha256: format_lower_hex(&validation_report_sha256),
                result_acknowledgement,
            },
        };
        persist_final_state(&self.output_directory, &final_state)?;
        Ok(HardwareCalibrationOutcome {
            output_directory: self.output_directory.clone(),
            artifact_sha256,
            validation_report_sha256,
            result_acknowledgement,
        })
    }

    fn await_result_acknowledgement(
        &mut self,
        instruction_revision: u64,
    ) -> Result<HardwareCalibrationResultAcknowledgement, HardwareCalibrationError> {
        let started = Instant::now();
        loop {
            let _ = self.server.poll_once()?;
            match result_acknowledgement_decision(
                started.elapsed(),
                self.app.result_acknowledgement_receipt()?,
            ) {
                ResultAcknowledgementDecision::Waiting => thread::sleep(LOOP_WAIT),
                ResultAcknowledgementDecision::Acknowledged(receipt) => {
                    if receipt.instruction_revision != instruction_revision {
                        return Err(invalid_data(
                            "saved result acknowledgement revision changed",
                        ));
                    }
                    return Ok(HardwareCalibrationResultAcknowledgement::Acknowledged {
                        instruction_revision,
                        acknowledged_host_time_ns: receipt.acknowledged_host_time_ns,
                    });
                }
                ResultAcknowledgementDecision::TimedOut => {
                    return Ok(HardwareCalibrationResultAcknowledgement::TimedOut {
                        instruction_revision,
                        timeout_seconds: RESULT_ACKNOWLEDGEMENT_TIMEOUT.as_secs(),
                    });
                }
            }
        }
    }

    fn commit_terminal(
        &mut self,
        terminal: &TerminalFailure,
    ) -> Result<(), HardwareCalibrationError> {
        if let Some(worker) = self.worker.as_mut() {
            worker.close_label_admission();
        }
        if !matches!(
            terminal.cause,
            HardwareCalibrationTerminalCause::Aborted
                | HardwareCalibrationTerminalCause::UnsupportedLayout
        ) {
            let _ = self
                .app
                .apply_hardware_event(HardwareCalibrationEvent::HardwareFault(
                    terminal.detail.clone(),
                ));
        }

        let mut incident_errors = Vec::new();
        let frozen_capture = self.frozen_capture.as_ref();
        if let Some(capture) = frozen_capture {
            let result = capture
                .worker
                .frozen_incident(terminal.cause)
                .map_err(|error| Box::new(error) as HardwareCalibrationError)
                .and_then(|incident| {
                    persist_incident(&self.output_directory, "incident", &incident)
                });
            if let Err(error) = result {
                incident_errors.push(error.to_string());
            }
            if !capture.display_receipts_persisted
                && let Err(error) = persist_display_receipts(
                    &self.output_directory,
                    capture.prefix,
                    &capture.display_receipts,
                )
            {
                incident_errors.push(error.to_string());
            }
        }
        let incident_prefix = if frozen_capture.is_some() {
            "monitor-incident"
        } else {
            "incident"
        };
        let active_result = self
            .worker
            .take()
            .ok_or_else(|| invalid_data("terminal attempt has no active capture worker"))
            .and_then(|worker| {
                worker
                    .close_incident(self.clock.now_ns(), terminal.cause)
                    .map_err(|error| Box::new(error) as HardwareCalibrationError)
            })
            .and_then(|incident| {
                persist_incident(&self.output_directory, incident_prefix, &incident)
            });
        if let Err(error) = active_result {
            incident_errors.push(error.to_string());
        }
        if !incident_errors.is_empty() {
            return Err(invalid_data(&format!(
                "{}; terminal incident persistence failed: {}",
                terminal.detail,
                incident_errors.join("; ")
            )));
        }
        let final_state = terminal_final_state(terminal, unix_time_ns()?);
        // This is the sole attempt commit marker and must be written after all
        // normal evidence or terminal incident files.
        persist_final_state(&self.output_directory, &final_state)?;
        self.frozen_capture = None;
        Ok(())
    }
}

fn take_timed_stage(
    stage: &mut CaptureStage,
    training: bool,
) -> Result<TimedSurfaceCapture, HardwareCalibrationError> {
    let replacement = CaptureStage::Complete;
    let current = std::mem::replace(stage, replacement);
    match (training, current) {
        (true, CaptureStage::SurfaceTraining(timed))
        | (false, CaptureStage::HeldOutValidation(timed)) => Ok(timed),
        (_, other) => {
            *stage = other;
            Err(invalid_data("surface stage changed unexpectedly"))
        }
    }
}

fn timed_capture_drained(timed: &TimedSurfaceCapture, now: u64) -> bool {
    let drain_deadline = timed
        .started_ns
        .saturating_add(timed.duration_ns)
        .saturating_add(timed.schedule_shift_ns)
        .saturating_add(PLAN_DRAIN_NS);
    now >= drain_deadline
        && timed.next_display == timed.displays.len()
        && timed.next_null == timed.nulls.len()
        && timed
            .displays
            .iter()
            .all(|display| display.pending.is_none())
        && timed.display_receipts.len() == timed.displays.len()
}

fn replay_keyboard_checkpoint(
    app: &CalibrationApp,
    bundle: &CalibrationCaptureBundle,
    precheck: &crate::calibration_engine::KeyboardPrecheckResult,
) -> Result<(), HardwareCalibrationError> {
    app.apply_hardware_event(HardwareCalibrationEvent::RejectedExamplesRecorded(
        bundle.evidence.rejected_examples(),
    ))?;
    app.apply_hardware_event(HardwareCalibrationEvent::BeginKeyboardPrecheck(
        BlockId::new(20),
    ))?;
    app.apply_hardware_event(HardwareCalibrationEvent::KeyboardPrecheck {
        metrics: precheck.metrics,
        evaluated_windows: precheck.evaluated_windows,
        weak_positions: precheck.weak_positions.clone(),
    })?;
    Ok(())
}

fn accepted_keyboard_observations(
    checkpoint: &KeyboardEvidenceCheckpoint,
) -> Vec<MatchedKeyboardObservation> {
    let mut selected = Vec::with_capacity(
        all_slots()
            .iter()
            .filter(|slot| keyboard_warm_start_eligible(slot.position))
            .count()
            * usize::from(KEYBOARD_EXAMPLES_PER_POSITION),
    );
    let mut used_window_ends = std::collections::BTreeSet::new();
    for position in all_slots()
        .iter()
        .map(|slot| slot.position)
        .filter(|position| keyboard_warm_start_eligible(*position))
    {
        let position_evidence = checkpoint
            .position(position)
            .expect("layout position exists in keyboard checkpoint");
        let target_count = usize::from(KEYBOARD_EXAMPLES_PER_POSITION);
        let required_blocks = position_evidence.matched_blocks.iter().take(2);
        let mut selected_windows = BTreeMap::new();
        for block in required_blocks {
            if let Some(observation) = checkpoint
                .observations()
                .iter()
                .find(|observation| {
                    observation.position == position && observation.block_id == *block
                })
                .copied()
                && used_window_ends.insert(observation.window_end_record)
            {
                selected_windows.insert(observation.window_end_record, observation);
            }
        }
        for observation in checkpoint
            .observations()
            .iter()
            .filter(|observation| observation.position == position)
            .copied()
        {
            if selected_windows.len() >= target_count {
                break;
            }
            if used_window_ends.insert(observation.window_end_record) {
                selected_windows.insert(observation.window_end_record, observation);
            }
        }
        selected.extend(selected_windows.into_values());
    }
    selected
}

fn replay_surface_fit(
    app: &CalibrationApp,
    lease: u64,
    bundle: &CalibrationCaptureBundle,
    artifact: &CalibrationArtifact,
) -> Result<(), HardwareCalibrationError> {
    for observation in bundle.evidence.observations().iter().filter(|observation| {
        observation.role() == EvidenceRole::Training
            && observation.domain() == EvidenceDomain::PassiveSurface
            && observation.position().is_some()
    }) {
        app.apply_hardware_event(HardwareCalibrationEvent::ExampleAccepted {
            domain: CalibrationDomain::PassiveSurface,
            position: observation.position().expect("filtered position exists"),
            block: BlockId::new(observation.block_id()),
            capture_lease_revision: lease,
        })?;
    }
    app.apply_hardware_event(HardwareCalibrationEvent::BeginArtifactFit(BlockId::new(40)))?;
    app.apply_hardware_event(HardwareCalibrationEvent::ArtifactLocked(
        artifact.locked_identity()?,
    ))?;
    Ok(())
}

fn spawn_artifact_fit(
    manifest: AcquisitionManifest,
    fitting_worker: Arc<CalibrationCaptureWorker>,
) -> Receiver<Result<FittedCalibration, HardwareCalibrationError>> {
    let (sender, receiver) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let result = (|| -> Result<FittedCalibration, HardwareCalibrationError> {
            let fitting = fitting_worker.derive_frozen()?;
            let artifact = fit_calibration(CalibrationFitInput {
                manifest: &manifest,
                clocks: &fitting.clocks,
                journal: &fitting.journal,
                session: &fitting.session,
                neutral_report: fitting
                    .neutral_report
                    .as_ref()
                    .ok_or_else(|| invalid_data("fitting bundle has no neutral report"))?,
            })?
            .artifact;
            Ok(FittedCalibration {
                bundle: fitting,
                artifact,
            })
        })();
        let _ = sender.send(result);
    });
    receiver
}

fn spawn_heldout_validation(
    manifest: AcquisitionManifest,
    artifact: CalibrationArtifact,
    heldout_worker: Arc<CalibrationCaptureWorker>,
) -> Receiver<Result<ValidatedCalibration, HardwareCalibrationError>> {
    let (sender, receiver) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let result = (|| -> Result<ValidatedCalibration, HardwareCalibrationError> {
            let heldout = heldout_worker.derive_frozen()?;
            let validation = validate_calibration(
                &manifest,
                &heldout.clocks,
                &artifact,
                &heldout.journal,
                &heldout.session,
            )?;
            Ok(ValidatedCalibration {
                bundle: heldout,
                validation,
            })
        })();
        let _ = sender.send(result);
    });
    receiver
}

fn timed_training_plan(
    plan: &SurfaceTrainingPlan,
    started_ns: u64,
    nominal_period_us: u32,
) -> TimedSurfaceCapture {
    timed_plan(
        plan.segments(),
        plan.scoring_targets(),
        plan.duration_ns(),
        started_ns,
        plan.canonical_json()
            .expect("canonical training plan serializes"),
        nominal_period_us,
    )
}

fn timed_heldout_plan(
    plan: &HeldOutSurfacePlan,
    started_ns: u64,
    nominal_period_us: u32,
) -> TimedSurfaceCapture {
    timed_plan(
        plan.segments(),
        plan.scoring_targets(),
        plan.duration_ns(),
        started_ns,
        plan.canonical_json()
            .expect("canonical held-out plan serializes"),
        nominal_period_us,
    )
}

fn timed_plan(
    segments: &[SurfacePromptSegment],
    targets: &[SurfaceTargetMetadata],
    duration_ns: u64,
    started_ns: u64,
    canonical_plan: Vec<u8>,
    nominal_period_us: u32,
) -> TimedSurfaceCapture {
    let target_by_id = targets
        .iter()
        .map(|target| (target.prompt_id().as_str(), target.position()))
        .collect::<BTreeMap<_, _>>();
    let mut displays = Vec::new();
    let mut nulls = Vec::new();
    let window_span_ns = u64::from(nominal_period_us)
        .saturating_mul(1_000)
        .saturating_mul(u64::try_from(WINDOW_FRAMES.saturating_sub(1)).unwrap_or(u64::MAX));
    for segment in segments {
        for presentation in segment.scheduled_presentations() {
            let (instruction, highlighted_position, prompted_position) = match presentation.phase()
            {
                SurfacePresentationPhase::Preparation => match segment.kind() {
                    SurfaceSegmentKind::PromptedTap | SurfaceSegmentKind::ThresholdTap => {
                        let position = target_by_id[segment.id().as_str()];
                        (
                            format!(
                                "Get ready to tap {} on the plain surface.",
                                label_for(position)
                            ),
                            Some(position),
                            None,
                        )
                    }
                    SurfaceSegmentKind::QuietNull => {
                        ("Return both hands to the home pose.".to_owned(), None, None)
                    }
                    SurfaceSegmentKind::ThresholdQuiet => {
                        let hand = match segment
                            .motion_hand()
                            .expect("threshold quiet segment names a hand")
                        {
                            Hand::Left => "left",
                            Hand::Right => "right",
                        };
                        (
                            format!("Keep your {hand} hand still in the home pose."),
                            None,
                            None,
                        )
                    }
                    SurfaceSegmentKind::OrdinaryMotion => {
                        let hand = match segment.motion_hand().expect("motion segment names a hand")
                        {
                            Hand::Left => "left",
                            Hand::Right => "right",
                        };
                        (
                            format!(
                                "Lift and gently rotate your {hand} hand, then return it without tapping."
                            ),
                            None,
                            None,
                        )
                    }
                },
                SurfacePresentationPhase::Cue => {
                    let position = target_by_id[segment.id().as_str()];
                    (
                        format!("Tap {} now.", label_for(position)),
                        Some(position),
                        Some(position),
                    )
                }
            };
            displays.push(TimedDisplay {
                at_ns: presentation.at_ns(),
                instruction,
                highlighted_position,
                prompted_position,
                prompt_id: segment.id().clone(),
                phase: presentation.phase(),
                pending: None,
            });
        }
        match segment.kind() {
            SurfaceSegmentKind::ThresholdQuiet => nulls.push(TimedNull {
                at_ns: segment.start_ns().saturating_add(window_span_ns.max(1)),
                prompt_id: segment.id().clone(),
                sample_ordinal: 0,
            }),
            SurfaceSegmentKind::QuietNull => {
                let at_ns = segment.start_ns().saturating_add(window_span_ns.max(1));
                for sample_ordinal in 0..2 {
                    nulls.push(TimedNull {
                        at_ns,
                        prompt_id: segment.id().clone(),
                        sample_ordinal,
                    });
                }
            }
            SurfaceSegmentKind::OrdinaryMotion => {
                let spacing = window_span_ns.max(1);
                let mut at_ns = segment.start_ns().saturating_add(spacing);
                let mut sample_ordinal = 0_u32;
                while at_ns < segment.end_ns() {
                    nulls.push(TimedNull {
                        at_ns,
                        prompt_id: segment.id().clone(),
                        sample_ordinal,
                    });
                    sample_ordinal = sample_ordinal.saturating_add(1);
                    at_ns = at_ns.saturating_add(spacing);
                }
            }
            SurfaceSegmentKind::PromptedTap | SurfaceSegmentKind::ThresholdTap => {}
        }
    }
    TimedSurfaceCapture {
        started_ns,
        duration_ns,
        displays,
        nulls,
        next_display: 0,
        next_null: 0,
        schedule_shift_ns: 0,
        display_receipts: Vec::new(),
        canonical_plan,
    }
}

fn keyboard_block(
    physical_code: u16,
    state: RawKeyState,
    prompt_attempt: u32,
    blocks_by_code: &mut BTreeMap<u16, u32>,
) -> u32 {
    match state {
        RawKeyState::Down => {
            let block = 10 + prompt_attempt % 2;
            blocks_by_code.insert(physical_code, block);
            block
        }
        RawKeyState::Up => blocks_by_code.remove(&physical_code).unwrap_or(10),
        RawKeyState::Repeat => blocks_by_code.get(&physical_code).copied().unwrap_or(10),
    }
}

fn keyboard_prompt_state(
    evidence: &KeyboardEvidenceCheckpoint,
    attempt: u32,
) -> KeyboardPromptState {
    let deficits = evidence.deficits(KEYBOARD_EXAMPLES_PER_POSITION);
    KeyboardPromptState::new(PromptCorpus::v1().next_keyboard_prompt(&deficits, attempt))
}

const fn health_check_frames(nominal_period_us: u32) -> u64 {
    HEALTH_CHECK_DURATION_US.div_ceil(nominal_period_us as u64)
}

fn label_for(position: PhysicalPosition) -> &'static str {
    slot_for_position(position)
        .expect("position belongs to the frozen layout")
        .label
}

fn persist_bundle(
    directory: &Path,
    prefix: &str,
    bundle: &CalibrationCaptureBundle,
) -> Result<(), HardwareCalibrationError> {
    write_new(
        &directory.join(format!("{prefix}-session.iks")),
        bundle.session.bytes(),
    )?;
    write_new(
        &directory.join(format!("{prefix}-clocks.json")),
        &bundle.clocks.canonical_json()?,
    )?;
    write_new(
        &directory.join(format!("{prefix}-capture.json")),
        &bundle.journal.canonical_json()?,
    )?;
    write_new(
        &directory.join(format!("{prefix}-evidence.json")),
        &bundle.evidence.canonical_json()?,
    )?;
    write_new(
        &directory.join(format!("{prefix}-matching-rule.json")),
        &bundle.capture_report.matching_rule.canonical_json()?,
    )?;
    if let Some(report) = &bundle.neutral_report {
        write_new(
            &directory.join(format!("{prefix}-neutral-report.json")),
            &report.canonical_json()?,
        )?;
    }
    Ok(())
}

fn persist_display_receipts(
    directory: &Path,
    prefix: &str,
    receipts: &[SurfaceDisplayReceipt],
) -> Result<(), HardwareCalibrationError> {
    write_new(
        &directory.join(format!("{prefix}-surface-display-receipts.json")),
        &serde_json::to_vec(receipts)?,
    )?;
    Ok(())
}

fn persist_incident(
    directory: &Path,
    prefix: &str,
    incident: &CalibrationCaptureIncident,
) -> Result<(), HardwareCalibrationError> {
    let session_result = write_new(
        &directory.join(format!("{prefix}-session.iks")),
        incident.session.bytes(),
    );
    let journal_result = write_new(
        &directory.join(format!("{prefix}-manifest.json")),
        incident.journal_bytes(),
    );
    match (session_result, journal_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(Box::new(error)),
        (Err(session_error), Err(journal_error)) => Err(invalid_data(&format!(
            "{prefix} session: {session_error}; {prefix} manifest: {journal_error}"
        ))),
    }
}

fn persist_final_state(
    directory: &Path,
    state: &HardwareCalibrationFinalState,
) -> Result<(), HardwareCalibrationError> {
    write_new(
        &directory.join("final-state.json"),
        &serde_json::to_vec(state)?,
    )?;
    Ok(())
}

fn terminal_final_state(
    terminal: &TerminalFailure,
    completed_unix_ns: u64,
) -> HardwareCalibrationFinalState {
    HardwareCalibrationFinalState {
        schema: HARDWARE_CALIBRATION_FINAL_STATE_SCHEMA.to_owned(),
        heldout_validation_passed: false,
        completed_unix_ns,
        disposition: HardwareCalibrationFinalDisposition::NonReady {
            cause: terminal.cause,
            detail: terminal.detail.clone(),
            incident_persisted: true,
            incident_error: None,
        },
    }
}

fn write_new(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    if let Some(directory) = path.parent() {
        File::open(directory)?.sync_all()?;
    }
    Ok(())
}

fn validate_paths(config: &HardwareCalibrationConfig) -> Result<(), HardwareCalibrationError> {
    if config.left_device == config.right_device {
        return Err(invalid_data("left and right controller paths must differ"));
    }
    if config.keyboard_device == config.left_device || config.keyboard_device == config.right_device
    {
        return Err(invalid_data("keyboard and controller paths must differ"));
    }
    if config.output_directory.try_exists()? {
        return Err(invalid_data(
            "hardware calibration output directory already exists",
        ));
    }
    Ok(())
}

fn bootstrap_manifest(
    template: AcquisitionManifestTemplate,
    input: &Receiver<SensorInput>,
    clock: &HostTimeOrigin,
    stop: &AtomicBool,
) -> Result<(AcquisitionManifest, Vec<DeviceTransportEvent>), HardwareCalibrationError> {
    let mut bootstrap = DeviceWireIdentityBootstrap::new(
        template.expected_controllers(),
        IdentityBootstrapConfig::default(),
    )?;
    while !bootstrap.is_complete() {
        if stop.load(Ordering::SeqCst) {
            return Err(invalid_data("controller identity bootstrap was stopped"));
        }
        match input.recv_timeout(Duration::from_millis(20)) {
            Ok(SensorInput::Chunk {
                source,
                receipt_time_ns,
                bytes,
            }) => bootstrap.handle(DeviceTransportEvent::Bytes {
                source,
                receipt_time_ns,
                bytes,
            })?,
            Ok(SensorInput::End { source, .. }) => {
                return Err(invalid_data(&format!(
                    "{source:?} controller ended before identity lock"
                )));
            }
            Ok(SensorInput::Failed {
                source,
                receipt_time_ns,
                error,
            }) => {
                return Err(invalid_data(&format!(
                    "{source:?} controller bootstrap read failed at {receipt_time_ns} ns: {error}"
                )));
            }
            Err(RecvTimeoutError::Timeout) => bootstrap.handle(DeviceTransportEvent::Tick {
                host_time_ns: clock.now_ns(),
            })?,
            Err(RecvTimeoutError::Disconnected) => {
                return Err(invalid_data(
                    "controller readers ended before identity lock",
                ));
            }
        }
    }
    let completed = bootstrap.finish()?;
    let manifest = template.lock(&completed.identities)?;
    Ok((manifest, completed.events))
}

fn spawn_sensor_readers(
    left: File,
    right: File,
    stop: Arc<AtomicBool>,
    clock: &HostTimeOrigin,
) -> Receiver<SensorInput> {
    let (sender, receiver) = mpsc::sync_channel(DEVICE_CHANNEL_CAPACITY);
    for (source, file) in [(Hand::Left, left), (Hand::Right, right)] {
        let sender = sender.clone();
        let stop = Arc::clone(&stop);
        let clock = clock.clone();
        thread::spawn(move || sensor_reader(source, file, sender, stop, clock));
    }
    drop(sender);
    receiver
}

fn sensor_reader(
    source: Hand,
    mut input: File,
    sender: SyncSender<SensorInput>,
    stop: Arc<AtomicBool>,
    clock: HostTimeOrigin,
) {
    let mut buffer = [0_u8; READ_BUFFER_BYTES];
    loop {
        match input.read(&mut buffer) {
            Ok(0) => {
                let _ = sender.send(SensorInput::End {
                    source,
                    receipt_time_ns: clock.now_ns(),
                });
                return;
            }
            Ok(length) => {
                if sender
                    .send(SensorInput::Chunk {
                        source,
                        receipt_time_ns: clock.now_ns(),
                        bytes: buffer[..length].to_vec(),
                    })
                    .is_err()
                {
                    return;
                }
                if stop.load(Ordering::SeqCst) {
                    let _ = sender.send(SensorInput::End {
                        source,
                        receipt_time_ns: clock.now_ns(),
                    });
                    return;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => {
                let _ = sender.send(SensorInput::Failed {
                    source,
                    receipt_time_ns: clock.now_ns(),
                    error,
                });
                return;
            }
        }
    }
}

fn spawn_keyboard_reader(
    mut keyboard: LinuxKeyboardDevice,
    stop: Arc<AtomicBool>,
    clock: &HostTimeOrigin,
    delivery: Arc<Mutex<()>>,
) -> Receiver<KeyboardInput> {
    let (sender, receiver) = mpsc::sync_channel(DEVICE_CHANNEL_CAPACITY);
    let clock = clock.clone();
    thread::spawn(move || {
        while !stop.load(Ordering::SeqCst) {
            match keyboard.read_event(&clock) {
                Ok(mut event) => {
                    let _delivery_guard = match delivery.lock() {
                        Ok(guard) => guard,
                        Err(_) => {
                            let _ = sender.send(KeyboardInput::Failed(io::Error::other(
                                "keyboard delivery boundary was poisoned",
                            )));
                            return;
                        }
                    };
                    // Ownership begins at lossless channel delivery, not in the
                    // preemptible gap after the blocking evdev read returns.
                    event.host_time_ns = clock.now_ns();
                    if sender.send(KeyboardInput::Event(event)).is_err() {
                        return;
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => {
                    let _ = sender.send(KeyboardInput::Failed(error));
                    return;
                }
            }
        }
    });
    receiver
}

fn unix_time_ns() -> Result<u64, HardwareCalibrationError> {
    Ok(u64::try_from(
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos(),
    )?)
}

fn random_session_id() -> Result<[u8; 16], HardwareCalibrationError> {
    let mut bytes = [0_u8; 16];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    if bytes == [0; 16] {
        return Err(invalid_data("random session identity was zero"));
    }
    Ok(bytes)
}

fn random_u64() -> Result<u64, HardwareCalibrationError> {
    let mut bytes = [0_u8; 8];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(u64::from_le_bytes(bytes))
}

fn invalid_data(message: &str) -> HardwareCalibrationError {
    Box::new(io::Error::new(
        io::ErrorKind::InvalidData,
        message.to_owned(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::Finger;

    fn dispatch_runner(keyboard_input: Receiver<KeyboardInput>) -> HardwareCalibrationRunner {
        use crate::manifest::{
            ACQUISITION_MANIFEST_SCHEMA, CORE28_LAYOUT_ID, HandManifest, ManifestHand,
            PROTOCOL_VERSION, SensorManifest, SensorRole, format_sha256,
        };

        let manifest = AcquisitionManifest {
            schema: ACQUISITION_MANIFEST_SCHEMA.to_owned(),
            layout_id: CORE28_LAYOUT_ID.to_owned(),
            protocol_version: PROTOCOL_VERSION,
            hands: [ManifestHand::Left, ManifestHand::Right]
                .into_iter()
                .enumerate()
                .map(|(hand_index, hand)| HandManifest {
                    hand,
                    controller_id: format!("dispatch-controller-{hand_index}"),
                    boot_id: format!("{:02x}", hand_index + 1).repeat(16),
                    sensors: std::array::from_fn(|index| SensorManifest {
                        sensor_index: u8::try_from(index).unwrap(),
                        mux_channel: u8::try_from(index).unwrap(),
                        role: [
                            SensorRole::Thumb,
                            SensorRole::Index,
                            SensorRole::Middle,
                            SensorRole::Ring,
                            SensorRole::Pinky,
                            SensorRole::Dorsal,
                        ][index],
                        mount_id: format!("mount-{hand_index}-{index}"),
                        wiring_id: format!("wire-{hand_index}-{index}"),
                        interrupt_pin: format!("gpio-{hand_index}-{index}"),
                        axis_map: [1, 0, 0, 0, 1, 0, 0, 0, 1],
                    }),
                })
                .collect(),
            controller_board: "fixture-board".to_owned(),
            firmware_build: "fixture-firmware".to_owned(),
            host_build: "fixture-host".to_owned(),
            sensor_model: "LSM6DS3TR-C".to_owned(),
            sensors_per_hand: 6,
            sensor_odr_hz: 208,
            accelerometer_range_g: 4,
            gyroscope_range_dps: 1_000,
            filter_profile: "gate0-raw-v1".to_owned(),
            i2c_bus_hz: 400_000,
            nominal_period_us: 5_000,
            prompt_corpus_sha256: format_sha256(&PromptCorpus::v1().sha256()),
            study_id: "keyboard-dispatch".to_owned(),
            split_id: "fitting".to_owned(),
            participant_alias: "fixture".to_owned(),
            consent_record: "fixture-consent".to_owned(),
            retention_policy: "local-test-only".to_owned(),
            rig_evidence_sha256: format_sha256(&[0xbb; 32]),
            axis_characterization_sha256: format_sha256(&[0xcc; 32]),
            calibration_artifact_sha256: None,
            calibration_validation_sha256: None,
        };
        let clock = HostTimeOrigin::new();
        let app = CalibrationApp::new(EvidenceMode::Hardware);
        app.apply_hardware_event(HardwareCalibrationEvent::Connected)
            .unwrap();
        let server =
            CalibrationHttpServer::bind_with_host_clock(app.clone(), 0, clock.clone()).unwrap();
        let (_sensor_sender, sensor_input) = mpsc::channel();
        let keyboard_source = KeyboardSourceIdentity {
            device_id: "dispatch-keyboard".to_owned(),
            name: "dispatch keyboard".to_owned(),
            physical_path: "fixture/dispatch".to_owned(),
            vendor_id: 1,
            product_id: 2,
        };
        let worker = CalibrationCaptureWorker::new(CalibrationCaptureWorkerConfig {
            manifest: manifest.clone(),
            keyboard_source: keyboard_source.clone(),
            purpose: EvidencePurpose::Fitting,
            matching_rule: None,
            session_id: [0x41; 16],
            created_unix_ns: 1_786_233_600_000_000_000,
            started_host_time_ns: 0,
            surface_plan: None,
            transport: DeviceTransportConfig::default(),
        })
        .unwrap();
        HardwareCalibrationRunner {
            app,
            server,
            stop: Arc::new(AtomicBool::new(false)),
            clock,
            sensor_input,
            keyboard_input,
            keyboard_delivery: Arc::new(Mutex::new(())),
            keyboard_source,
            selected_keyboard_state: SelectedKeyboardState::default(),
            manifest,
            output_directory: PathBuf::new(),
            worker: Some(worker),
            frozen_capture: None,
            stage: CaptureStage::HardwareCheck,
            capture_lease_revision: None,
            keyboard_layout_confirmation: None,
            created_unix_ns: 1_786_233_600_000_000_000,
            fitting_artifact: None,
            validation_report_sha256: None,
            terminal_failure: None,
        }
    }

    fn dispatch_event(code: u16, state: RawKeyState) -> KeyboardInput {
        KeyboardInput::Event(RawKeyboardEvent {
            device_id: "dispatch-keyboard".to_owned(),
            physical_code: code,
            position: None,
            modifier: false,
            state,
            kernel_time_ns: None,
            host_time_ns: u64::from(code),
        })
    }

    #[test]
    fn keyboard_dispatch_yields_with_replenished_backlog_and_loses_no_events() {
        // A real unbounded channel makes the replenished, always-readable queue
        // deterministic: no producer scheduling race can make it briefly empty.
        // Production storage is bounded, but deliveries during dispatch can
        // supply exactly this same FIFO sequence.
        let (sender, receiver) = mpsc::channel();
        let mut runner = dispatch_runner(receiver);
        for state in [RawKeyState::Down, RawKeyState::Up] {
            for code in 0..u16::try_from(DEVICE_CHANNEL_CAPACITY).unwrap() {
                sender.send(dispatch_event(code, state)).unwrap();
            }
        }
        // Exercise the real loop: it services the sensor tick and advance's
        // elapsed-time/deadline work despite the always-readable backlog.
        // Complete makes one loop turn terminate without a calibration fit.
        let (_sensor_sender, sensor_input) = mpsc::channel();
        runner.sensor_input = sensor_input;
        runner.stage = CaptureStage::Complete;
        runner.run_active().unwrap();
        assert_eq!(
            runner.selected_keyboard_state.held_codes.len(),
            DEVICE_CHANNEL_CAPACITY
        );
        let revision = runner.app.snapshot().unwrap().revision;
        runner
            .app
            .apply_action(
                crate::calibration_app::CalibrationAction::Abort,
                revision,
                runner.clock.now_ns(),
            )
            .unwrap();
        assert!(runner.run_active().is_err());
        assert_eq!(
            runner.terminal_failure.as_ref().unwrap().cause,
            HardwareCalibrationTerminalCause::Aborted,
        );
        assert_eq!(
            runner.selected_keyboard_state.held_codes.len(),
            DEVICE_CHANNEL_CAPACITY
        );

        // Replenish before the next dispatch. The original ups must be delivered
        // first, not discarded to make space or overtaken by the new downs.
        for code in 0..u16::try_from(DEVICE_CHANNEL_CAPACITY).unwrap() {
            sender
                .send(dispatch_event(code, RawKeyState::Down))
                .unwrap();
        }
        runner.drain_keyboard().unwrap();
        assert!(runner.selected_keyboard_state.is_neutral());
        runner.drain_keyboard().unwrap();
        assert_eq!(
            runner.selected_keyboard_state.held_codes.len(),
            DEVICE_CHANNEL_CAPACITY
        );
        assert!(matches!(
            runner.keyboard_input.try_recv(),
            Err(TryRecvError::Empty)
        ));
    }

    #[test]
    fn contended_keyboard_boundary_dispatches_only_one_batch_before_retry() {
        let (sender, receiver) = mpsc::channel();
        let mut runner = dispatch_runner(receiver);
        for code in 0..u16::try_from(2 * DEVICE_CHANNEL_CAPACITY).unwrap() {
            sender
                .send(dispatch_event(code, RawKeyState::Down))
                .unwrap();
        }
        let delivery = Arc::clone(&runner.keyboard_delivery);
        let _producer_guard = match delivery.lock() {
            Ok(guard) => guard,
            Err(_) => panic!("test delivery gate was poisoned"),
        };
        assert_eq!(
            runner
                .try_keyboard_delivery_boundary(|_| -> Result<(), HardwareCalibrationError> {
                    panic!("a contended boundary must not transition");
                })
                .unwrap(),
            None,
        );
        assert_eq!(
            runner.selected_keyboard_state.held_codes.len(),
            DEVICE_CHANNEL_CAPACITY
        );
        runner.drain_keyboard().unwrap();
        assert_eq!(
            runner.selected_keyboard_state.held_codes.len(),
            2 * DEVICE_CHANNEL_CAPACITY
        );
    }

    #[test]
    fn quiescent_keyboard_boundary_drains_the_full_bounded_queue_before_transition() {
        let (sender, receiver) = mpsc::sync_channel(DEVICE_CHANNEL_CAPACITY);
        let mut runner = dispatch_runner(receiver);
        {
            let _producer_guard = match runner.keyboard_delivery.lock() {
                Ok(guard) => guard,
                Err(_) => panic!("test delivery gate was poisoned"),
            };
            for _ in 0..DEVICE_CHANNEL_CAPACITY / 2 {
                sender.send(dispatch_event(30, RawKeyState::Down)).unwrap();
                sender.send(dispatch_event(30, RawKeyState::Up)).unwrap();
            }
        }
        assert_eq!(
            runner
                .try_keyboard_delivery_boundary(|runner| {
                    assert!(runner.selected_keyboard_state.is_neutral());
                    assert!(matches!(
                        runner.keyboard_input.try_recv(),
                        Err(TryRecvError::Empty)
                    ));
                    assert!(matches!(
                        runner.keyboard_delivery.try_lock(),
                        Err(TryLockError::WouldBlock)
                    ));
                    runner.stage = CaptureStage::Complete;
                    Ok(true)
                })
                .unwrap(),
            Some(true)
        );
        {
            let _producer_guard = match runner.keyboard_delivery.lock() {
                Ok(guard) => guard,
                Err(_) => panic!("test delivery gate was poisoned"),
            };
            sender.send(dispatch_event(30, RawKeyState::Down)).unwrap();
        }
        runner.drain_keyboard().unwrap();
        assert!(!runner.selected_keyboard_state.is_neutral());
        assert!(matches!(runner.stage, CaptureStage::Complete));
    }

    #[test]
    fn keyboard_blocks_alternate_only_between_finite_prompt_passes() {
        let mut active = BTreeMap::new();

        assert_eq!(keyboard_block(30, RawKeyState::Down, 0, &mut active), 10);
        assert_eq!(keyboard_block(30, RawKeyState::Repeat, 99, &mut active), 10);
        assert_eq!(keyboard_block(30, RawKeyState::Up, 99, &mut active), 10);
        assert_eq!(keyboard_block(30, RawKeyState::Down, 1, &mut active), 11);
    }

    #[test]
    fn selected_keyboard_state_survives_worker_boundaries_until_key_up() {
        let mut state = SelectedKeyboardState::default();
        let mut event = RawKeyboardEvent {
            device_id: "fixture-keyboard".to_owned(),
            physical_code: 30,
            position: None,
            modifier: false,
            state: RawKeyState::Down,
            kernel_time_ns: None,
            host_time_ns: 10,
        };
        state.observe(&event);
        assert!(!state.is_neutral());

        // Worker replacement does not touch this runner-owned state.
        event.state = RawKeyState::Up;
        event.host_time_ns = 11;
        state.observe(&event);
        assert!(state.is_neutral());
    }

    #[test]
    fn finite_keyboard_warm_start_and_passive_threshold_plans_cover_their_contracts() {
        assert_eq!(health_check_frames(5_000), 1_000);
        assert!(!KeyboardEvidenceCheckpoint::empty().is_complete(KEYBOARD_EXAMPLES_PER_POSITION));
        let evidence = KeyboardEvidenceCheckpoint::from_observations(
            all_slots()
                .iter()
                .filter(|slot| keyboard_warm_start_eligible(slot.position))
                .flat_map(|slot| {
                    (0..KEYBOARD_EXAMPLES_PER_POSITION).map(move |example| {
                        MatchedKeyboardObservation {
                            position: slot.position,
                            block_id: if example < KEYBOARD_EXAMPLES_PER_POSITION / 2 {
                                10
                            } else {
                                11
                            },
                            window_end_record: u64::try_from(
                                position_index(slot.position)
                                    .expect("layout position has an index"),
                            )
                            .unwrap_or(u64::MAX)
                            .saturating_mul(100)
                            .saturating_add(u64::from(example)),
                        }
                    })
                }),
        );
        assert!(evidence.is_complete(KEYBOARD_EXAMPLES_PER_POSITION));

        let plan = SurfaceTrainingPlan::v1();
        let threshold_ids = plan
            .segments()
            .iter()
            .filter(|segment| segment.kind() == SurfaceSegmentKind::ThresholdTap)
            .map(|segment| segment.id().as_str())
            .collect::<std::collections::BTreeSet<_>>();
        let targets = plan
            .scoring_targets()
            .iter()
            .filter(|target| threshold_ids.contains(target.prompt_id().as_str()))
            .map(SurfaceTargetMetadata::position)
            .collect::<Vec<_>>();
        assert_eq!(targets.len(), 10);
        for hand in [Hand::Left, Hand::Right] {
            for finger in [
                Finger::Thumb,
                Finger::Index,
                Finger::Middle,
                Finger::Ring,
                Finger::Pinky,
            ] {
                assert_eq!(
                    targets
                        .iter()
                        .filter(|position| position.hand == hand && position.finger == finger)
                        .count(),
                    1
                );
            }
        }
        assert!(
            targets
                .iter()
                .filter(|position| position.finger == Finger::Thumb)
                .all(|position| !keyboard_warm_start_eligible(*position))
        );
    }

    #[test]
    fn displayed_coverage_uses_a_canonical_complete_set_from_two_blocks() {
        let position = all_slots()
            .iter()
            .find(|slot| keyboard_warm_start_eligible(slot.position))
            .expect("layout has an addressable position")
            .position;
        let first_block = (0..8).map(|window_end_record| MatchedKeyboardObservation {
            position,
            block_id: 10,
            window_end_record,
        });
        let evidence = KeyboardEvidenceCheckpoint::from_observations(first_block.chain(
            std::iter::once(MatchedKeyboardObservation {
                position,
                block_id: 11,
                window_end_record: 8,
            }),
        ));
        let accepted = accepted_keyboard_observations(&evidence);
        assert_eq!(accepted.len(), usize::from(KEYBOARD_EXAMPLES_PER_POSITION));
        assert!(
            accepted
                .iter()
                .any(|observation| observation.block_id == 10)
        );
        assert!(
            accepted
                .iter()
                .any(|observation| observation.block_id == 11)
        );
        assert_eq!(
            accepted
                .iter()
                .map(|observation| observation.window_end_record)
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            accepted.len(),
        );
        assert_eq!(
            evidence
                .position(position)
                .expect("position evidence exists")
                .matched_examples,
            9,
        );
    }

    #[test]
    fn timed_plans_preserve_every_target_and_per_hand_motion_exposure() {
        let training_plan = SurfaceTrainingPlan::v1();
        let training = timed_training_plan(&training_plan, 2_000_000_000, 5_000);
        assert_eq!(
            training
                .displays
                .iter()
                .filter(|display| display.prompted_position.is_some())
                .count(),
            training_plan.scoring_targets().len()
        );
        assert_eq!(training.duration_ns, training_plan.duration_ns());
        assert!(!training.canonical_plan.is_empty());
        assert_eq!(training.schedule_shift_ns, 0);
        assert!(training.display_receipts.is_empty());
        assert_eq!(
            training.displays.len(),
            training_plan
                .segments()
                .iter()
                .map(|segment| segment.scheduled_presentations().count())
                .sum::<usize>(),
            "every schedule-shifting presentation enters the timed capture"
        );
        assert!(
            training
                .displays
                .iter()
                .all(|display| display.pending.is_none())
        );
        assert!(
            training
                .displays
                .iter()
                .filter(|display| { display.prompted_position.is_some() })
                .all(|display| {
                    display.instruction.starts_with("Tap ")
                        && display.instruction.ends_with(" now.")
                })
        );
        assert!(
            training
                .displays
                .iter()
                .any(|display| { display.instruction.starts_with("Get ready to tap ") })
        );

        let heldout_plan = HeldOutSurfacePlan::v1([0x5a; 32], 73);
        let nominal_period_us = 5_000;
        let heldout = timed_heldout_plan(&heldout_plan, 4_000_000_000, nominal_period_us);
        assert_eq!(
            heldout
                .displays
                .iter()
                .filter(|display| display.prompted_position.is_some())
                .count(),
            heldout_plan.scoring_targets().len()
        );
        assert!(
            heldout
                .nulls
                .windows(2)
                .all(|pair| pair[0].at_ns <= pair[1].at_ns)
        );

        let window_span_ns = u64::from(nominal_period_us)
            * 1_000
            * u64::try_from(WINDOW_FRAMES - 1).expect("window length fits u64");
        let required_ns = crate::validation::MIN_HELD_OUT_ORDINARY_MOTION_EXPOSURE_MS * 1_000_000;
        let ordinary_motion_hand_by_id = heldout_plan
            .segments()
            .iter()
            .filter(|segment| segment.kind() == SurfaceSegmentKind::OrdinaryMotion)
            .map(|segment| {
                (
                    segment.id().as_str(),
                    segment
                        .motion_hand()
                        .expect("ordinary-motion plan segment names one hand"),
                )
            })
            .collect::<BTreeMap<_, _>>();
        for hand in [Hand::Left, Hand::Right] {
            let windows = heldout
                .nulls
                .iter()
                .filter(|action| {
                    ordinary_motion_hand_by_id.get(action.prompt_id.as_str()) == Some(&hand)
                })
                .count();
            assert!(
                u64::try_from(windows).expect("window count fits u64") * window_span_ns
                    >= required_ns
            );
        }
    }

    #[test]
    fn raw_prompt_attempts_do_not_change_matched_deficits_and_stop_at_the_cap() {
        let mut training = KeyboardTrainingState::new();
        let first_instruction = training.prompt.rendered();
        let expected = training
            .prompt
            .current()
            .expect("the first prompt has a position");
        let index = position_index(expected).expect("prompt position belongs to the layout");
        let wrong = all_slots()
            .iter()
            .map(|slot| slot.position)
            .find(|position| keyboard_warm_start_eligible(*position) && *position != expected)
            .expect("Core-28 has another addressable position");
        assert!(!training.prompt.accept(wrong));
        assert_eq!(training.prompt.rendered(), first_instruction);
        let original_deficits = training.evidence.deficits(KEYBOARD_EXAMPLES_PER_POSITION);
        training.raw_attempts[index] = MAX_EXAMPLES_PER_POSITION;
        assert!(training.evidence.deficits(KEYBOARD_EXAMPLES_PER_POSITION) == original_deficits);
        assert_eq!(training.capped_deficit(), Some(expected));
    }

    #[test]
    fn timed_capture_needs_every_receipt_and_null_window_after_the_drain_horizon() {
        let mut timed = timed_training_plan(&SurfaceTrainingPlan::v1(), 2_000_000_000, 5_000);
        let after_drain = timed
            .started_ns
            .saturating_add(timed.duration_ns)
            .saturating_add(PLAN_DRAIN_NS);
        assert!(!timed_capture_drained(&timed, after_drain));

        timed.next_display = timed.displays.len();
        timed.next_null = timed.nulls.len();
        timed.display_receipts = timed
            .displays
            .iter()
            .enumerate()
            .map(|(index, display)| SurfaceDisplayReceipt {
                prompt_id: display.prompt_id.clone(),
                phase: display.phase,
                planned_offset_ns: display.at_ns,
                published_at_ns: display.at_ns,
                acknowledged_at_ns: display.at_ns,
                instruction_revision: u64::try_from(index).unwrap_or(u64::MAX) + 1,
                prompted_position: display.prompted_position,
            })
            .collect();
        assert!(timed_capture_drained(&timed, after_drain));

        timed.displays[0].pending = Some(PendingSurfaceInstruction {
            revision: 1,
            published_at_ns: timed.started_ns,
        });
        assert!(!timed_capture_drained(&timed, after_drain));
    }

    #[test]
    fn abort_lease_loss_and_source_fault_commit_typed_nonready_state() {
        for (index, cause) in [
            HardwareCalibrationTerminalCause::Aborted,
            HardwareCalibrationTerminalCause::CaptureLeaseLost,
            HardwareCalibrationTerminalCause::SourceFault,
        ]
        .into_iter()
        .enumerate()
        {
            let directory = std::env::temp_dir().join(format!(
                "invisible-keyboard-terminal-state-{}-{index}",
                std::process::id()
            ));
            std::fs::create_dir(&directory).expect("isolated terminal-state directory is new");
            let terminal = TerminalFailure::new(cause, "test terminal cause");
            let state = terminal_final_state(&terminal, 1_786_233_600_000_000_000);
            persist_final_state(&directory, &state).expect("final state is durably committed");

            let persisted: HardwareCalibrationFinalState = serde_json::from_slice(
                &std::fs::read(directory.join("final-state.json"))
                    .expect("final state can be read back"),
            )
            .expect("final state remains typed JSON");
            assert!(!persisted.heldout_validation_passed);
            assert!(matches!(
                persisted.disposition,
                HardwareCalibrationFinalDisposition::NonReady {
                    cause: persisted_cause,
                    incident_persisted: true,
                    incident_error: None,
                    ..
                } if persisted_cause == cause
            ));
            std::fs::remove_dir_all(&directory).expect("isolated terminal-state directory removes");
        }
    }

    fn incident_runner(purpose: EvidencePurpose) -> HardwareCalibrationRunner {
        let (_sender, receiver) = mpsc::channel();
        let mut runner = dispatch_runner(receiver);
        runner.output_directory = std::env::temp_dir().join(format!(
            "invisible-keyboard-frozen-{}-{}",
            std::process::id(),
            random_u64().unwrap()
        ));
        std::fs::create_dir(&runner.output_directory).unwrap();
        runner
            .app
            .apply_hardware_event(HardwareCalibrationEvent::HardwareChecked)
            .unwrap();
        let identified = runner
            .app
            .apply_hardware_event(HardwareCalibrationEvent::KeyboardDeviceIdentified {
                device: runner.keyboard_source.name.clone(),
            })
            .unwrap();
        let revision = identified.keyboard_layout_instruction_revision.unwrap();
        runner
            .app
            .decide_keyboard_layout(
                crate::calibration_app::KeyboardLayoutDecision::ConfirmUsQwerty,
                revision,
                2,
            )
            .unwrap();
        runner.capture_lease_revision = runner
            .app
            .activate_confirmed_layout_capture(revision)
            .unwrap()
            .capture_lease_revision;

        let transport = DeviceTransportConfig {
            merge_lateness_ns: 10,
            source_silence_timeout_ns: u64::MAX / 2,
            max_pending_candidates: 4_096,
        };
        let mut neutral_worker = CalibrationCaptureWorker::new(CalibrationCaptureWorkerConfig {
            manifest: runner.manifest.clone(),
            keyboard_source: runner.keyboard_source.clone(),
            purpose: EvidencePurpose::Fitting,
            matching_rule: None,
            session_id: [0x41; 16],
            created_unix_ns: runner.created_unix_ns,
            started_host_time_ns: 0,
            surface_plan: None,
            transport,
        })
        .unwrap();
        retain_still_frames(&mut neutral_worker, &runner.manifest);
        let windows = vec![NeutralRecordWindow {
            block_id: "neutral-original".to_owned(),
            window_id: "still-original".to_owned(),
            role: NeutralWindowRole::Neutral,
            start_record: 0,
            end_record_exclusive: neutral_worker.record_count(),
        }];
        neutral_worker.set_neutral_windows(windows.clone()).unwrap();
        if purpose == EvidencePurpose::HeldOutValidation {
            neutral_worker.freeze(2_000_000_000).unwrap();
            let neutral_session = neutral_worker
                .frozen_incident(CalibrationIncidentReason::Aborted)
                .unwrap()
                .session
                .into_admitted()
                .unwrap();
            let rule = crate::calibration_capture::derive_matching_rule(
                &runner.manifest,
                &neutral_session,
                &windows,
            )
            .unwrap();
            let mut worker = CalibrationCaptureWorker::new(CalibrationCaptureWorkerConfig {
                manifest: runner.manifest.clone(),
                keyboard_source: runner.keyboard_source.clone(),
                purpose,
                matching_rule: Some(rule),
                session_id: [0x42; 16],
                created_unix_ns: runner.created_unix_ns,
                started_host_time_ns: 0,
                surface_plan: Some(
                    CalibrationSurfacePlanBinding::held_out(
                        &HeldOutSurfacePlan::v1([0x5a; 32], 73),
                        3_000_000_000,
                    )
                    .unwrap(),
                ),
                transport,
            })
            .unwrap();
            retain_still_frames(&mut worker, &runner.manifest);
            runner.worker = Some(worker);
        } else {
            let context =
                KeyboardCaptureContext::new(EvidenceRole::Training, EvidenceDomain::Keyboard, 10);
            for (state, host_time_ns) in [(RawKeyState::Down, 1), (RawKeyState::Up, 2)] {
                neutral_worker
                    .handle_keyboard(
                        &runner.keyboard_source,
                        RawKeyboardEvent {
                            device_id: runner.keyboard_source.device_id.clone(),
                            physical_code: crate::labels::LEFT_SHIFT_PHYSICAL_CODE,
                            position: None,
                            modifier: true,
                            state,
                            kernel_time_ns: None,
                            host_time_ns,
                        },
                        context,
                    )
                    .unwrap();
            }
            runner.worker = Some(neutral_worker);
        }
        runner
            .worker_mut()
            .unwrap()
            .confirm_keyboard_layout(KeyboardLayoutConfirmationReceipt::us_qwerty(1, 2))
            .unwrap();
        runner
    }

    fn retain_still_frames(worker: &mut CalibrationCaptureWorker, manifest: &AcquisitionManifest) {
        let configs = [Hand::Left, Hand::Right].map(|hand| {
            let identity = manifest.hands[hand as usize].frame_identity().unwrap();
            crate::simulation::SyntheticConfig::new(hand, 17)
                .with_noise(0)
                .with_shared_motion(0)
                .with_wire_identity(identity.controller_id_sha256, identity.boot_id)
        });
        for trial in 0..3 {
            let windows = configs.each_ref().map(|config| config.idle_window(trial));
            for index in 0..WINDOW_FRAMES {
                for hand in [Hand::Left, Hand::Right] {
                    let mut frame = windows[hand as usize].frames()[index];
                    frame.device_time_us = trial * 250_000 + frame.device_time_us % 1_000_000;
                    for (sensor_index, sensor) in frame.sensors.iter_mut().enumerate() {
                        sensor.acceleration = [0, 0, 16_000 + (index % 3) as i16 - 1];
                        sensor.angular_velocity = [0, 0, 0];
                        sensor.sample_time_offset_us = sensor_index as i16 * 20;
                    }
                    worker
                        .handle_sensor(DeviceTransportEvent::Bytes {
                            source: hand,
                            receipt_time_ns: 1_000_000_000 + frame.device_time_us * 1_000,
                            bytes: crate::transport::encode_transport_frame(&frame).unwrap(),
                        })
                        .unwrap();
                }
            }
        }
        worker
            .handle_sensor(DeviceTransportEvent::Tick {
                host_time_ns: 2_000_000_000,
            })
            .unwrap();
    }

    fn freeze_missing_contact(
        runner: &mut HardwareCalibrationRunner,
        purpose: EvidencePurpose,
    ) -> Arc<CalibrationCaptureWorker> {
        let started_ns = 3_000_000_000;
        let (prefix, binding, mut timed) = if purpose == EvidencePurpose::Fitting {
            let plan = SurfaceTrainingPlan::v1();
            (
                "fitting",
                CalibrationSurfacePlanBinding::fitting(started_ns).unwrap(),
                timed_training_plan(&plan, started_ns, runner.manifest.nominal_period_us),
            )
        } else {
            let plan = HeldOutSurfacePlan::v1([0x5a; 32], 73);
            (
                "heldout",
                CalibrationSurfacePlanBinding::held_out(&plan, started_ns).unwrap(),
                timed_heldout_plan(&plan, started_ns, runner.manifest.nominal_period_us),
            )
        };
        runner
            .worker_mut()
            .unwrap()
            .bind_surface_plan(binding)
            .unwrap();
        let mut receipts = Vec::new();
        let mut next_null = 0;
        for (index, display) in timed.displays.iter().enumerate() {
            while let Some(null) = timed.nulls.get(next_null)
                && null.at_ns < display.at_ns
            {
                runner
                    .worker_mut()
                    .unwrap()
                    .record_idle(
                        null.prompt_id.clone(),
                        null.sample_ordinal,
                        started_ns + null.at_ns,
                    )
                    .unwrap();
                next_null += 1;
            }
            let time = started_ns + display.at_ns;
            let revision = index as u64 + 1;
            runner
                .worker_mut()
                .unwrap()
                .record_display_acknowledgement(
                    display.prompt_id.clone(),
                    SurfacePresentationReceipt::new(time, time, time, revision).unwrap(),
                )
                .unwrap();
            receipts.push(SurfaceDisplayReceipt {
                prompt_id: display.prompt_id.clone(),
                phase: display.phase,
                planned_offset_ns: display.at_ns,
                published_at_ns: time,
                acknowledged_at_ns: time,
                instruction_revision: revision,
                prompted_position: display.prompted_position,
            });
        }
        for null in &timed.nulls[next_null..] {
            runner
                .worker_mut()
                .unwrap()
                .record_idle(
                    null.prompt_id.clone(),
                    null.sample_ordinal,
                    started_ns + null.at_ns,
                )
                .unwrap();
        }
        let finished_ns = started_ns + timed.duration_ns + PLAN_DRAIN_NS;
        let frozen = if purpose == EvidencePurpose::Fitting {
            timed.next_display = timed.displays.len();
            timed.next_null = timed.nulls.len();
            timed.display_receipts = receipts;
            let (_sender, receiver) = mpsc::channel();
            runner.keyboard_input = receiver;
            runner.stage = CaptureStage::SurfaceTraining(timed);
            runner.advance_surface_training(finished_ns).unwrap();
            assert!(matches!(runner.stage, CaptureStage::ArtifactFitting { .. }));
            Arc::clone(&runner.frozen_capture.as_ref().unwrap().worker)
        } else {
            runner
                .freeze_capture(finished_ns, prefix, receipts)
                .unwrap()
        };
        for hand in [Hand::Left, Hand::Right] {
            let identity = runner.manifest.hands[hand as usize]
                .frame_identity()
                .unwrap();
            let window = crate::simulation::SyntheticConfig::new(hand, 23)
                .with_wire_identity(identity.controller_id_sha256, identity.boot_id)
                .idle_window(3);
            runner
                .handle_worker_sensor(DeviceTransportEvent::Bytes {
                    source: hand,
                    receipt_time_ns: finished_ns + 1_000,
                    bytes: crate::transport::encode_transport_frame(&window.frames()[0]).unwrap(),
                })
                .unwrap();
        }
        frozen
    }

    fn assert_original_incident(
        runner: &HardwareCalibrationRunner,
        expected: &CalibrationCaptureIncident,
        expected_receipts: &[u8],
        purpose: EvidencePurpose,
        cause: CalibrationIncidentReason,
    ) {
        let directory = &runner.output_directory;
        assert_eq!(
            std::fs::read(directory.join("incident-session.iks")).unwrap(),
            expected.session.bytes()
        );
        let actual: serde_json::Value = serde_json::from_slice(
            &std::fs::read(directory.join("incident-manifest.json")).unwrap(),
        )
        .unwrap();
        let original: serde_json::Value = serde_json::from_slice(expected.journal_bytes()).unwrap();
        for field in [
            "session_id",
            "session_sha256",
            "purpose",
            "events",
            "retained_records",
        ] {
            assert_eq!(
                actual[field], original[field],
                "{field} belongs to the original interval"
            );
        }
        assert_eq!(actual["evidence_eligible"], false);
        let monitor: serde_json::Value = serde_json::from_slice(
            &std::fs::read(directory.join("monitor-incident-manifest.json")).unwrap(),
        )
        .unwrap();
        assert_ne!(monitor["session_id"], actual["session_id"]);
        assert_eq!(monitor["retained_records"], 2);
        assert_eq!(monitor["events"], serde_json::json!([]));
        assert_eq!(monitor["evidence_eligible"], false);
        assert_ne!(
            std::fs::read(directory.join("monitor-incident-session.iks")).unwrap(),
            expected.session.bytes()
        );
        let prefix = if purpose == EvidencePurpose::Fitting {
            "fitting"
        } else {
            "heldout"
        };
        assert_eq!(
            std::fs::read(directory.join(format!("{prefix}-surface-display-receipts.json")))
                .unwrap(),
            expected_receipts
        );
        for filename in [
            "calibration-artifact.json",
            "validation-report.json",
            "fitting-evidence.json",
            "heldout-evidence.json",
        ] {
            assert!(
                !directory.join(filename).exists(),
                "{filename} cannot grant authority"
            );
        }
        let final_state: HardwareCalibrationFinalState =
            serde_json::from_slice(&std::fs::read(directory.join("final-state.json")).unwrap())
                .unwrap();
        assert!(!final_state.heldout_validation_passed);
        assert!(matches!(
            final_state.disposition,
            HardwareCalibrationFinalDisposition::NonReady {
                cause: actual_cause, incident_persisted: true, incident_error: None, ..
            } if actual_cause == cause
        ));
        assert_ne!(
            runner.app.control_state().unwrap().phase,
            CalibrationPhase::ValidationPassed
        );
    }

    #[test]
    fn missing_passive_contact_preserves_original_fitting_and_heldout_capture() {
        for purpose in [EvidencePurpose::Fitting, EvidencePurpose::HeldOutValidation] {
            let mut runner = incident_runner(purpose);
            let worker = freeze_missing_contact(&mut runner, purpose);
            let expected = worker
                .frozen_incident(CalibrationIncidentReason::ProcessingFailure)
                .unwrap();
            let receipts =
                serde_json::to_vec(&runner.frozen_capture.as_ref().unwrap().display_receipts)
                    .unwrap();
            let error = worker
                .derive_frozen()
                .err()
                .expect("no passive tap was delivered");
            assert!(matches!(
                &error,
                CalibrationWorkerError::Capture(CalibrationCaptureError::NoSurfaceContact { .. })
            ));
            if purpose == EvidencePurpose::Fitting {
                let CaptureStage::ArtifactFitting { result } =
                    std::mem::replace(&mut runner.stage, CaptureStage::Complete)
                else {
                    panic!("drained fitting capture must start the background fit");
                };
                let derived = result.recv().unwrap();
                let (sender, result) = mpsc::sync_channel(1);
                sender.send(derived).unwrap();
                runner.stage = CaptureStage::ArtifactFitting { result };
                assert!(runner.advance_artifact_fitting(0).is_err());
            } else {
                let (sender, result) = mpsc::sync_channel(1);
                sender
                    .send(Err(Box::new(error) as HardwareCalibrationError))
                    .unwrap();
                runner.stage = CaptureStage::HeldOutValidating { result };
                assert!(runner.advance_heldout_validation().is_err());
            }
            let terminal = TerminalFailure::new(
                CalibrationIncidentReason::ProcessingFailure,
                "missing passive contact",
            );
            runner.commit_terminal(&terminal).unwrap();
            assert_original_incident(&runner, &expected, &receipts, purpose, terminal.cause);
            std::fs::remove_dir_all(&runner.output_directory).unwrap();
        }
    }

    #[test]
    fn fitting_and_heldout_monitors_reject_acquisition_faults_and_keep_both_incidents() {
        for purpose in [EvidencePurpose::Fitting, EvidencePurpose::HeldOutValidation] {
            for hand in [Hand::Left, Hand::Right] {
                for defect in 0..10 {
                    let mut runner = incident_runner(purpose);
                    let prefix = if purpose == EvidencePurpose::Fitting {
                        "fitting"
                    } else {
                        "heldout"
                    };
                    let frozen = runner
                        .freeze_capture(2_000_000_000, prefix, Vec::new())
                        .unwrap();
                    let expected = frozen
                        .frozen_incident(CalibrationIncidentReason::StreamCorruption)
                        .unwrap();
                    // Pause a real frozen-worker derivation at its consuming
                    // thread boundary, without injecting a successful result.
                    let (release, background) = if purpose == EvidencePurpose::Fitting {
                        let (release, result, background) =
                            pending_capture_result(Arc::clone(&frozen));
                        runner.stage = CaptureStage::ArtifactFitting { result };
                        (release, background)
                    } else {
                        let (release, result, background) =
                            pending_capture_result(Arc::clone(&frozen));
                        runner.stage = CaptureStage::HeldOutValidating { result };
                        (release, background)
                    };
                    let identity = runner.manifest.hands[hand as usize]
                        .frame_identity()
                        .unwrap();
                    let mut bad = crate::simulation::SyntheticConfig::new(hand, 17)
                        .with_wire_identity(identity.controller_id_sha256, identity.boot_id)
                        .idle_window(3)
                        .frames()[0];
                    bad.device_time_us = 750_000;
                    for (index, sensor) in bad.sensors.iter_mut().enumerate() {
                        sensor.sample_time_offset_us = index as i16 * 20;
                    }
                    match defect {
                        0 => bad.valid_sensor_mask &= !1,
                        1 => bad.flags |= crate::protocol::FLAG_CLOCK_UNSYNCED,
                        2 => bad.flags |= crate::protocol::FLAG_ACQUISITION_OVERRUN,
                        3 => bad.sensors[5].sample_time_offset_us = 5_001,
                        4 => bad.sequence = bad.sequence.wrapping_add(1),
                        5 => bad.device_time_us = 574_999,
                        6 => {
                            bad.device_time_us = 575_001;
                            bad.sensors[0].sample_time_offset_us = -2;
                        }
                        7 => {
                            bad.device_time_us = 0;
                            bad.sensors[0].sample_time_offset_us = -1;
                        }
                        8 => {
                            bad.device_time_us = 580_000;
                            for sensor in &mut bad.sensors {
                                sensor.sample_time_offset_us = 0;
                            }
                            bad.sensors[0].sample_time_offset_us = -5_000;
                        }
                        9 => {
                            bad.device_time_us = 575_000;
                            for (index, sensor) in bad.sensors.iter_mut().enumerate() {
                                sensor.sample_time_offset_us = index as i16 * 20 + 1;
                            }
                        }
                        _ => unreachable!(),
                    }
                    let mut clean = bad;
                    clean.sequence = 49;
                    clean.device_time_us = 755_000;
                    clean.valid_sensor_mask = crate::protocol::ALL_SENSORS_VALID;
                    clean.flags = 0;
                    for (index, sensor) in clean.sensors.iter_mut().enumerate() {
                        sensor.sample_time_offset_us = index as i16 * 20;
                    }
                    let mut bytes = crate::transport::encode_transport_frame(&bad).unwrap();
                    bytes.extend(crate::transport::encode_transport_frame(&clean).unwrap());
                    let first = runner
                        .handle_worker_sensor(DeviceTransportEvent::Bytes {
                            source: hand,
                            receipt_time_ns: 2_000_000_100,
                            bytes,
                        })
                        .expect_err(
                            "the monitor must reject bad completed frames before merge release",
                        )
                        .to_string();
                    assert!(
                        runner
                            .handle_worker_sensor(DeviceTransportEvent::Bytes {
                                source: hand,
                                receipt_time_ns: 2_000_000_200,
                                bytes: crate::transport::encode_transport_frame(&clean).unwrap(),
                            })
                            .is_err()
                    );
                    assert_eq!(runner.terminal_failure.as_ref().unwrap().detail, first);
                    let advanced = if purpose == EvidencePurpose::Fitting {
                        runner.advance_artifact_fitting(2_000_000_201)
                    } else {
                        runner.advance_heldout_validation()
                    };
                    assert!(
                        advanced.is_err(),
                        "later computation cannot erase an observed stream fault"
                    );
                    assert!(!matches!(runner.stage, CaptureStage::Complete));
                    assert!(Arc::ptr_eq(
                        &frozen,
                        &runner.frozen_capture.as_ref().unwrap().worker
                    ));
                    let terminal = runner.terminal_failure.take().unwrap();
                    assert_eq!(terminal.cause, CalibrationIncidentReason::StreamCorruption);
                    runner.commit_terminal(&terminal).unwrap();
                    assert_original_incident(&runner, &expected, b"[]", purpose, terminal.cause);
                    let monitor: serde_json::Value = serde_json::from_slice(
                        &std::fs::read(
                            runner
                                .output_directory
                                .join("monitor-incident-manifest.json"),
                        )
                        .unwrap(),
                    )
                    .unwrap();
                    assert_eq!(monitor["retention_complete"], true);
                    assert_eq!(monitor["source_close_complete"], true);
                    assert_eq!(monitor["retained_events"], 0);
                    assert!(monitor["terminal_fault"].is_string());
                    let session = crate::session::FinalizedSession::audit(
                        std::fs::read(runner.output_directory.join("monitor-incident-session.iks"))
                            .unwrap(),
                    )
                    .unwrap();
                    let mut reader = session.reader();
                    let records = [
                        reader.next_record().unwrap().unwrap(),
                        reader.next_record().unwrap().unwrap(),
                    ];
                    assert_eq!(
                        records[0].payload,
                        crate::protocol::encode_frame(&bad).unwrap()
                    );
                    assert_eq!(
                        records[1].payload,
                        crate::protocol::encode_frame(&clean).unwrap()
                    );
                    assert!(reader.next_record().unwrap().is_none());
                    runner.stage = CaptureStage::Complete;
                    release.send(()).unwrap();
                    assert!(
                        background.join().unwrap(),
                        "terminal closure drops computation authority"
                    );
                    std::fs::remove_dir_all(&runner.output_directory).unwrap();
                }
            }
        }
    }

    #[test]
    fn monitor_successor_keeps_sequence_and_sensor_time_continuity() {
        for hand in [Hand::Left, Hand::Right] {
            for defect in 0..4 {
                let mut runner = incident_runner(EvidencePurpose::Fitting);
                runner
                    .freeze_capture(2_000_000_000, "fitting", Vec::new())
                    .unwrap();
                let mut successor = runner.new_monitor_worker(2_000_000_000).unwrap();
                runner
                    .worker_mut()
                    .unwrap()
                    .transfer_transport_to(&mut successor)
                    .unwrap();
                runner.worker = Some(successor);
                let identity = runner.manifest.hands[hand as usize]
                    .frame_identity()
                    .unwrap();
                let mut bad = crate::simulation::SyntheticConfig::new(hand, 17)
                    .with_wire_identity(identity.controller_id_sha256, identity.boot_id)
                    .idle_window(3)
                    .frames()[0];
                for (index, sensor) in bad.sensors.iter_mut().enumerate() {
                    sensor.sample_time_offset_us = index as i16 * 20;
                }
                match defect {
                    0 => {
                        bad.device_time_us = 750_000;
                        bad.sequence = bad.sequence.wrapping_add(1);
                    }
                    1 => {
                        bad.device_time_us = 575_001;
                        bad.sensors[0].sample_time_offset_us = -2;
                    }
                    2 => {
                        bad.device_time_us = 580_000;
                        for sensor in &mut bad.sensors {
                            sensor.sample_time_offset_us = 0;
                        }
                        bad.sensors[0].sample_time_offset_us = -5_000;
                    }
                    3 => {
                        bad.device_time_us = 575_000;
                        for (index, sensor) in bad.sensors.iter_mut().enumerate() {
                            sensor.sample_time_offset_us = index as i16 * 20 + 1;
                        }
                    }
                    _ => unreachable!(),
                }
                assert!(
                    runner
                        .handle_worker_sensor(DeviceTransportEvent::Bytes {
                            source: hand,
                            receipt_time_ns: 2_000_000_100,
                            bytes: crate::transport::encode_transport_frame(&bad).unwrap(),
                        })
                        .is_err(),
                    "the monitor-to-held-out handoff cannot reset stream continuity"
                );
                let terminal = runner.terminal_failure.take().unwrap();
                runner.commit_terminal(&terminal).unwrap();
                std::fs::remove_dir_all(&runner.output_directory).unwrap();
            }
        }
    }

    #[test]
    fn observed_monitor_fault_cannot_be_erased_by_a_transport_handoff() {
        let mut runner = incident_runner(EvidencePurpose::Fitting);
        let frozen = runner
            .freeze_capture(2_000_000_000, "fitting", Vec::new())
            .unwrap();
        let identity = runner.manifest.hands[0].frame_identity().unwrap();
        let mut bad = crate::simulation::SyntheticConfig::new(Hand::Left, 17)
            .with_wire_identity(identity.controller_id_sha256, identity.boot_id)
            .idle_window(3)
            .frames()[0];
        bad.flags |= crate::protocol::FLAG_ACQUISITION_OVERRUN;
        let first = runner
            .handle_worker_sensor(DeviceTransportEvent::Bytes {
                source: Hand::Left,
                receipt_time_ns: 2_000_000_100,
                bytes: crate::transport::encode_transport_frame(&bad).unwrap(),
            })
            .unwrap_err()
            .to_string();
        let mut successor = runner.new_monitor_worker(2_000_000_100).unwrap();
        assert!(
            runner
                .worker_mut()
                .unwrap()
                .transfer_transport_to(&mut successor)
                .is_err()
        );
        assert!(
            runner
                .freeze_capture(2_000_000_101, "heldout", Vec::new())
                .is_err()
        );
        assert!(Arc::ptr_eq(
            &frozen,
            &runner.frozen_capture.as_ref().unwrap().worker
        ));
        assert_eq!(runner.terminal_failure.as_ref().unwrap().detail, first);
        assert_eq!(runner.worker_mut().unwrap().record_count(), 1);
        let terminal = runner.terminal_failure.take().unwrap();
        runner.commit_terminal(&terminal).unwrap();
        assert!(
            !runner
                .output_directory
                .join("calibration-artifact.json")
                .exists()
        );
        assert!(
            !runner
                .output_directory
                .join("validation-report.json")
                .exists()
        );
        std::fs::remove_dir_all(&runner.output_directory).unwrap();
    }

    fn pending_capture_result<T: Send + 'static>(
        worker: Arc<CalibrationCaptureWorker>,
    ) -> (
        SyncSender<()>,
        Receiver<Result<T, HardwareCalibrationError>>,
        thread::JoinHandle<bool>,
    ) {
        let (release, wait) = mpsc::sync_channel(1);
        let (sender, result) = mpsc::sync_channel(1);
        let background = thread::spawn(move || {
            wait.recv().unwrap();
            let error = worker.derive_frozen().err().unwrap();
            sender
                .send(Err(Box::new(error) as HardwareCalibrationError))
                .is_err()
        });
        (release, result, background)
    }

    #[test]
    fn abort_and_lease_loss_preserve_original_capture_when_result_pending_or_dropped() {
        for purpose in [EvidencePurpose::Fitting, EvidencePurpose::HeldOutValidation] {
            for dropped in [false, true] {
                for (action, cause) in [
                    (
                        crate::calibration_app::CalibrationAction::Abort,
                        CalibrationIncidentReason::Aborted,
                    ),
                    (
                        crate::calibration_app::CalibrationAction::Pause,
                        CalibrationIncidentReason::CaptureLeaseLost,
                    ),
                ] {
                    let mut runner = incident_runner(purpose);
                    let worker = freeze_missing_contact(&mut runner, purpose);
                    let expected = worker.frozen_incident(cause).unwrap();
                    let receipts = serde_json::to_vec(
                        &runner.frozen_capture.as_ref().unwrap().display_receipts,
                    )
                    .unwrap();
                    let (release, background) = if purpose == EvidencePurpose::Fitting {
                        let (release, result, background) = pending_capture_result(worker);
                        runner.stage = CaptureStage::ArtifactFitting { result };
                        (release, background)
                    } else {
                        let (release, result, background) = pending_capture_result(worker);
                        runner.stage = CaptureStage::HeldOutValidating { result };
                        (release, background)
                    };
                    if dropped {
                        runner.stage = CaptureStage::Complete;
                    }
                    let revision = runner.app.snapshot().unwrap().revision;
                    runner
                        .app
                        .apply_action(action, revision, runner.clock.now_ns())
                        .unwrap();
                    assert!(runner.run_active().is_err());
                    let terminal = runner.terminal_failure.take().unwrap();
                    assert_eq!(terminal.cause, cause);
                    runner.commit_terminal(&terminal).unwrap();
                    assert_original_incident(&runner, &expected, &receipts, purpose, cause);
                    runner.stage = CaptureStage::Complete;
                    release.send(()).unwrap();
                    assert!(background.join().unwrap());
                    std::fs::remove_dir_all(&runner.output_directory).unwrap();
                }
            }
        }
    }

    #[test]
    fn original_incident_file_conflict_prevents_terminal_commit_but_retains_other_evidence() {
        let purpose = EvidencePurpose::Fitting;
        let mut runner = incident_runner(purpose);
        let worker = freeze_missing_contact(&mut runner, purpose);
        let expected = worker
            .frozen_incident(CalibrationIncidentReason::Aborted)
            .unwrap();
        write_new(
            &runner.output_directory.join("incident-session.iks"),
            b"existing file",
        )
        .unwrap();
        let terminal =
            TerminalFailure::new(CalibrationIncidentReason::Aborted, "original abort cause");
        let error = runner.commit_terminal(&terminal).unwrap_err().to_string();
        assert!(error.contains("original abort cause"));
        assert!(error.contains("terminal incident persistence failed"));
        assert!(!runner.output_directory.join("final-state.json").exists());
        assert_eq!(
            std::fs::read(runner.output_directory.join("incident-session.iks")).unwrap(),
            b"existing file"
        );
        assert!(
            runner
                .output_directory
                .join("monitor-incident-session.iks")
                .exists()
        );
        assert!(
            runner
                .output_directory
                .join("monitor-incident-manifest.json")
                .exists()
        );
        assert!(
            runner
                .output_directory
                .join("fitting-surface-display-receipts.json")
                .exists()
        );
        assert_eq!(
            std::fs::read(runner.output_directory.join("incident-manifest.json")).unwrap(),
            expected.journal_bytes()
        );
        assert!(
            runner.frozen_capture.is_some(),
            "failed persistence keeps original ownership"
        );
        assert!(
            !runner
                .output_directory
                .join("calibration-artifact.json")
                .exists()
        );
        assert!(
            !runner
                .output_directory
                .join("validation-report.json")
                .exists()
        );
        std::fs::remove_dir_all(&runner.output_directory).unwrap();
    }

    #[test]
    fn healthy_freeze_preserves_interleaved_receipt_order() {
        let (_sender, receiver) = mpsc::channel();
        let runner = dispatch_runner(receiver);
        let mut worker = CalibrationCaptureWorker::new(CalibrationCaptureWorkerConfig {
            manifest: runner.manifest.clone(),
            keyboard_source: runner.keyboard_source.clone(),
            purpose: EvidencePurpose::Fitting,
            matching_rule: None,
            session_id: [0x43; 16],
            created_unix_ns: runner.created_unix_ns,
            started_host_time_ns: 0,
            surface_plan: None,
            transport: DeviceTransportConfig {
                merge_lateness_ns: 100,
                source_silence_timeout_ns: 1_000,
                max_pending_candidates: 64,
            },
        })
        .unwrap();
        let mut expected = Vec::new();
        for (hand, time, index) in [
            (Hand::Right, 100, 0),
            (Hand::Left, 101, 0),
            (Hand::Right, 102, 1),
        ] {
            let identity = runner.manifest.hands[hand as usize]
                .frame_identity()
                .unwrap();
            let frame = crate::simulation::SyntheticConfig::new(hand, 23)
                .with_wire_identity(identity.controller_id_sha256, identity.boot_id)
                .idle_window(0)
                .frames()[index];
            expected.push((
                crate::session::ControllerStream::from(hand),
                time,
                crate::protocol::encode_frame(&frame).unwrap().to_vec(),
            ));
            worker
                .handle_sensor(DeviceTransportEvent::Bytes {
                    source: hand,
                    receipt_time_ns: time,
                    bytes: crate::transport::encode_transport_frame(&frame).unwrap(),
                })
                .unwrap();
        }
        assert_eq!(worker.record_count(), 0);
        worker.freeze(200).unwrap();
        let session = worker
            .frozen_incident(CalibrationIncidentReason::Aborted)
            .unwrap()
            .session
            .into_admitted()
            .unwrap();
        let mut reader = session.reader();
        let mut actual = Vec::new();
        while let Some(record) = reader.next_record().unwrap() {
            actual.push((record.source, record.host_time_ns, record.payload));
        }
        assert_eq!(
            actual, expected,
            "healthy closure preserves receipt merge order"
        );
    }

    #[test]
    fn capture_transition_preserves_a_split_frame_and_pending_candidates() {
        let (_sender, receiver) = mpsc::channel();
        let mut runner = dispatch_runner(receiver);
        let manifest = runner.manifest.clone();
        let frame = |hand: Hand, index: usize| {
            let identity = manifest.hands[hand as usize].frame_identity().unwrap();
            crate::simulation::SyntheticConfig::new(hand, 29)
                .with_wire_identity(identity.controller_id_sha256, identity.boot_id)
                .idle_window(0)
                .frames()[index]
        };
        for hand in [Hand::Left, Hand::Right] {
            runner
                .handle_worker_sensor(DeviceTransportEvent::Bytes {
                    source: hand,
                    receipt_time_ns: 1_000_000_000,
                    bytes: crate::transport::encode_transport_frame(&frame(hand, 0)).unwrap(),
                })
                .unwrap();
        }
        runner
            .handle_worker_sensor(DeviceTransportEvent::Tick {
                host_time_ns: 1_050_000_001,
            })
            .unwrap();
        assert_eq!(runner.worker_mut().unwrap().record_count(), 2);
        let split_frame = crate::transport::encode_transport_frame(&frame(Hand::Left, 1)).unwrap();
        let split = split_frame.len() / 2;
        runner
            .handle_worker_sensor(DeviceTransportEvent::Bytes {
                source: Hand::Left,
                receipt_time_ns: 1_060_000_000,
                bytes: split_frame[..split].to_vec(),
            })
            .unwrap();
        runner
            .handle_worker_sensor(DeviceTransportEvent::Bytes {
                source: Hand::Right,
                receipt_time_ns: 1_060_000_000,
                bytes: crate::transport::encode_transport_frame(&frame(Hand::Right, 1)).unwrap(),
            })
            .unwrap();
        let original = runner
            .freeze_capture(1_060_000_000, "fitting", Vec::new())
            .expect("an OS read boundary must not terminate a sensor frame");
        let original = original
            .frozen_incident(CalibrationIncidentReason::Aborted)
            .unwrap();
        assert_eq!(original.session.summary().total_records, 2);
        assert_eq!(original.session.summary().invalid_records, 0);
        runner
            .handle_worker_sensor(DeviceTransportEvent::Bytes {
                source: Hand::Left,
                receipt_time_ns: 1_065_000_000,
                bytes: split_frame[split..].to_vec(),
            })
            .unwrap();
        let monitor = runner.worker_mut().unwrap();
        monitor.freeze(1_070_000_000).unwrap();
        let monitor = monitor
            .frozen_incident(CalibrationIncidentReason::Aborted)
            .unwrap();
        let session = monitor.session.into_admitted().unwrap();
        let original = original.session.into_admitted().unwrap();
        assert_ne!(session.header().session_id, original.header().session_id);
        let mut reader = session.reader();
        for (hand, time) in [(Hand::Right, 1_060_000_000), (Hand::Left, 1_065_000_000)] {
            let record = reader.next_record().unwrap().unwrap();
            assert_eq!(record.source, crate::session::ControllerStream::from(hand));
            assert_eq!(record.host_time_ns, time);
            assert_eq!(
                record.payload,
                crate::protocol::encode_frame(&frame(hand, 1)).unwrap()
            );
        }
        assert!(
            reader.next_record().unwrap().is_none(),
            "each pending frame appears exactly once"
        );
    }
}
