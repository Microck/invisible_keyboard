//! Authoritative dual-controller and selected-keyboard calibration capture.
//!
//! This module owns the join between raw controller transport, physical
//! keyboard events, and immutable calibration sidecars. The browser may show
//! progress, pause capture, or abort, but it cannot create labels or claim that
//! a sensor event occurred.

use std::{collections::BTreeSet, error::Error, fmt};

use serde::{Deserialize, Serialize};

use crate::{
    artifact::KeyboardLayoutConfirmationReceipt,
    calibration_capture::{
        CalibrationCaptureError, CalibrationCaptureInput, CalibrationCaptureJournal,
        CalibrationMatchingRule, CalibrationSurfacePlanBinding, CaptureBuildReport,
        CapturedCalibrationEvent, SurfacePresentationReceipt, build_calibration_evidence_qualified,
        match_calibration_capture_qualified,
    },
    calibration_engine::{CalibrationEvidence, EvidenceDomain, EvidencePurpose, EvidenceRole},
    clock_epoch::{ClockEpochFitError, ClockEpochSet, QualifiedOfflineClocks},
    decoder::{AbstainReason, FrameIntegrity, FrameIntegrityError, WindowError},
    device_transport::{
        CandidateIdentity, DeviceTransport, DeviceTransportConfig,
        DeviceTransportConfigurationError, DeviceTransportError, DeviceTransportEvent,
        DeviceTransportFault, DeviceTransportUpdate, SourcedTransportCandidate,
    },
    labels::{KeyboardLabelFilter, KeyboardSourceIdentity, LabelDisposition, RawKeyboardEvent},
    layout::{
        Hand, PHYSICAL_POSITION_COUNT, PhysicalPosition, all_slots, keyboard_warm_start_eligible,
        position_index,
    },
    manifest::{AcquisitionManifest, ManifestError, format_sha256, sha256},
    neutral::{NeutralEvidenceError, NeutralEvidenceReport, NeutralRecordWindow},
    prompts::PromptId,
    session::{
        ClosedSession, ControllerStream, FinalizedSession, SessionError, SessionHeader,
        SessionWriter,
    },
    transport::TransportCandidate,
};

pub const CALIBRATION_INCIDENT_SCHEMA: &str = "invisible-keyboard-calibration-incident-v1";
const MAX_RETAINED_RECORDS: usize = 300_000;
const MAX_RETAINED_RECORD_PAYLOAD_BYTES: usize = 64 * 1_024 * 1_024;
const MAX_RETAINED_EVENTS: usize = 20_000;
const MAX_KEYBOARD_IDENTITY_BYTES: usize = 1_024;
const MAX_CALIBRATION_INCIDENT_BYTES: usize = 32 * 1_024 * 1_024;

/// Immutable provenance and bounds for one fitting or held-out capture.
pub struct CalibrationCaptureWorkerConfig {
    pub manifest: AcquisitionManifest,
    pub keyboard_source: KeyboardSourceIdentity,
    pub purpose: EvidencePurpose,
    pub matching_rule: Option<CalibrationMatchingRule>,
    pub session_id: [u8; 16],
    pub created_unix_ns: u64,
    pub started_host_time_ns: u64,
    pub surface_plan: Option<CalibrationSurfacePlanBinding>,
    pub transport: DeviceTransportConfig,
}

/// Trusted metadata attached before one selected-keyboard event is read.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct KeyboardCaptureContext {
    pub role: EvidenceRole,
    pub domain: EvidenceDomain,
    pub block_id: u32,
    include_as_candidate: bool,
}

impl KeyboardCaptureContext {
    #[must_use]
    pub const fn new(role: EvidenceRole, domain: EvidenceDomain, block_id: u32) -> Self {
        Self {
            role,
            domain,
            block_id,
            include_as_candidate: true,
        }
    }

    /// Retain this key event for state/provenance without creating a label.
    #[must_use]
    pub const fn state_only(mut self) -> Self {
        self.include_as_candidate = false;
        self
    }
}

/// Complete immutable output from one capture stage.
pub struct CalibrationCaptureBundle {
    pub session: FinalizedSession,
    pub clocks: ClockEpochSet,
    pub journal: CalibrationCaptureJournal,
    pub evidence: CalibrationEvidence,
    pub capture_report: CaptureBuildReport,
    pub neutral_report: Option<NeutralEvidenceReport>,
}

/// Typed reason why a capture closed as incident material instead of evidence.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CalibrationIncidentReason {
    Aborted,
    UnsupportedLayout,
    CaptureLeaseLost,
    SourceFault,
    StreamCorruption,
    TimeCap,
    ProcessingFailure,
}

/// Bounded diagnostic journal for a capture that must never enter fitting.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CalibrationIncidentJournal {
    schema: String,
    evidence_eligible: bool,
    reason: CalibrationIncidentReason,
    purpose: EvidencePurpose,
    acquisition_manifest_sha256: String,
    session_sha256: String,
    session_id: [u8; 16],
    created_unix_ns: u64,
    closed_host_time_ns: u64,
    retained_records: u64,
    retained_events: u64,
    source_close_complete: bool,
    retention_complete: bool,
    terminal_fault: Option<String>,
    close_errors: Vec<String>,
    events: Vec<CapturedCalibrationEvent>,
}

impl CalibrationIncidentJournal {
    #[must_use]
    pub const fn reason(&self) -> CalibrationIncidentReason {
        self.reason
    }

    #[must_use]
    pub const fn evidence_eligible(&self) -> bool {
        self.evidence_eligible
    }

    #[must_use]
    pub const fn source_close_complete(&self) -> bool {
        self.source_close_complete
    }

    /// Whether every owned input fit the bounded diagnostic retention limits.
    /// Parser closure alone does not establish complete evidence retention.
    #[must_use]
    pub const fn retention_complete(&self) -> bool {
        self.retention_complete
    }

    #[must_use]
    pub fn terminal_fault(&self) -> Option<&str> {
        self.terminal_fault.as_deref()
    }

    #[must_use]
    pub fn close_errors(&self) -> &[String] {
        &self.close_errors
    }

    #[must_use]
    pub const fn retained_records(&self) -> u64 {
        self.retained_records
    }

    #[must_use]
    pub const fn retained_events(&self) -> u64 {
        self.retained_events
    }

    pub fn canonical_json(&self) -> Result<Vec<u8>, CalibrationWorkerError> {
        let bytes = serde_json::to_vec(self)
            .map_err(|error| CalibrationWorkerError::IncidentSerialization(error.to_string()))?;
        if bytes.len() > MAX_CALIBRATION_INCIDENT_BYTES {
            return Err(CalibrationWorkerError::IncidentTooLarge {
                actual: bytes.len(),
            });
        }
        Ok(bytes)
    }
}

/// Footer-closed incident material. It intentionally contains no clocks,
/// evidence, matching rule, fitted artifact, or validation report.
pub struct CalibrationCaptureIncident {
    pub session: ClosedSession,
    pub journal: CalibrationIncidentJournal,
    canonical_journal: Vec<u8>,
}

impl CalibrationCaptureIncident {
    /// Canonical bounded journal bytes validated when the incident closed.
    #[must_use]
    pub fn journal_bytes(&self) -> &[u8] {
        &self.canonical_journal
    }
}

/// One synchronized physical-keyboard observation in a live evidence prefix.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct MatchedKeyboardObservation {
    pub(crate) position: PhysicalPosition,
    pub(crate) block_id: u32,
    pub(crate) window_end_record: u64,
}

/// Exact matched coverage for one physical-keyboard position.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KeyboardPositionEvidence {
    pub position: PhysicalPosition,
    pub matched_examples: u32,
    pub matched_blocks: Vec<u32>,
}

/// Non-consuming, evidence-derived keyboard coverage for a released prefix.
///
/// Raw key events are intentionally absent. Every item here has already been
/// joined one-to-one to a valid sensor window by the canonical matcher.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KeyboardEvidenceCheckpoint {
    positions: Vec<KeyboardPositionEvidence>,
    observations: Vec<MatchedKeyboardObservation>,
}

impl KeyboardEvidenceCheckpoint {
    #[must_use]
    pub(crate) fn empty() -> Self {
        Self {
            positions: all_slots()
                .iter()
                .map(|slot| KeyboardPositionEvidence {
                    position: slot.position,
                    matched_examples: 0,
                    matched_blocks: Vec::new(),
                })
                .collect(),
            observations: Vec::new(),
        }
    }

    pub(crate) fn from_observations(
        observations: impl IntoIterator<Item = MatchedKeyboardObservation>,
    ) -> Self {
        let mut checkpoint = Self::empty();
        let mut blocks: [BTreeSet<u32>; PHYSICAL_POSITION_COUNT] =
            std::array::from_fn(|_| BTreeSet::new());
        for observation in observations {
            let index = position_index(observation.position)
                .expect("evidence position belongs to the layout");
            checkpoint.positions[index].matched_examples = checkpoint.positions[index]
                .matched_examples
                .saturating_add(1);
            blocks[index].insert(observation.block_id);
            checkpoint.observations.push(observation);
        }
        for (position, position_blocks) in checkpoint.positions.iter_mut().zip(blocks) {
            position.matched_blocks = position_blocks.into_iter().collect();
        }
        checkpoint
    }

    #[must_use]
    pub fn position(&self, position: PhysicalPosition) -> Option<&KeyboardPositionEvidence> {
        position_index(position).and_then(|index| self.positions.get(index))
    }

    #[must_use]
    pub(crate) fn observations(&self) -> &[MatchedKeyboardObservation] {
        &self.observations
    }

    /// Return prompt weights for count and separated-block shortfalls.
    #[must_use]
    pub fn deficits(&self, required_examples: u8) -> [u16; PHYSICAL_POSITION_COUNT] {
        std::array::from_fn(|index| {
            if !keyboard_warm_start_eligible(self.positions[index].position) {
                return 0;
            }
            let matched = u8::try_from(self.positions[index].matched_examples).unwrap_or(u8::MAX);
            let count_deficit = required_examples.saturating_sub(matched);
            let block_deficit = if self.positions[index].matched_blocks.len() < 2 {
                required_examples
            } else {
                0
            };
            u16::from(count_deficit) + u16::from(block_deficit)
        })
    }

    #[must_use]
    pub fn is_complete(&self, required_examples: u8) -> bool {
        self.positions.iter().all(|position| {
            !keyboard_warm_start_eligible(position.position)
                || (position.matched_examples >= u32::from(required_examples)
                    && position.matched_blocks.len() >= 2)
        })
    }
}

/// Single owner for transport decoding, raw-key state, and evidence assembly.
pub struct CalibrationCaptureWorker {
    manifest: AcquisitionManifest,
    manifest_sha256: [u8; 32],
    keyboard_source: KeyboardSourceIdentity,
    keyboard_layout_confirmation: Option<KeyboardLayoutConfirmationReceipt>,
    keyboard_filter: KeyboardLabelFilter,
    purpose: EvidencePurpose,
    inherited_matching_rule: Option<CalibrationMatchingRule>,
    session_id: [u8; 16],
    created_unix_ns: u64,
    started_host_time_ns: u64,
    surface_plan: Option<CalibrationSurfacePlanBinding>,
    transport: DeviceTransport,
    connected: [bool; 2],
    last_sensor_receipt_ns: [Option<u64>; 2],
    frame_integrity: [FrameIntegrity; 2],
    records: Vec<SourcedTransportCandidate>,
    retained_record_payload_bytes: usize,
    valid_frame_counts: [u64; 2],
    events: Vec<CapturedCalibrationEvent>,
    neutral_windows: Vec<NeutralRecordWindow>,
    last_host_time_ns: u64,
    terminal_fault: Option<CalibrationWorkerFault>,
    label_admission_open: bool,
    frozen: bool,
    retention_complete: bool,
    close_errors: Vec<String>,
}

impl CalibrationCaptureWorker {
    /// Start one capture after the acquisition manifest has locked both boots.
    pub fn new(config: CalibrationCaptureWorkerConfig) -> Result<Self, CalibrationWorkerError> {
        config
            .manifest
            .validate()
            .map_err(CalibrationWorkerError::Manifest)?;
        if config.manifest.hands.len() != 2
            || config.session_id == [0; 16]
            || config.created_unix_ns == 0
            || config.keyboard_source.device_id.trim().is_empty()
            || config.keyboard_source.name.trim().is_empty()
            || config.keyboard_source.physical_path.trim().is_empty()
            || config
                .keyboard_source
                .device_id
                .len()
                .saturating_add(config.keyboard_source.name.len())
                .saturating_add(config.keyboard_source.physical_path.len())
                > MAX_KEYBOARD_IDENTITY_BYTES
        {
            return Err(CalibrationWorkerError::InvalidConfiguration);
        }
        match (config.purpose, &config.matching_rule) {
            (EvidencePurpose::Fitting, None) | (EvidencePurpose::HeldOutValidation, Some(_)) => {}
            _ => return Err(CalibrationWorkerError::InvalidMatchingRuleUse),
        }
        if let Some(rule) = &config.matching_rule {
            rule.validate().map_err(CalibrationWorkerError::Capture)?;
        }
        if let Some(surface_plan) = &config.surface_plan {
            surface_plan
                .validate(config.purpose)
                .map_err(CalibrationWorkerError::Capture)?;
        }
        if config.purpose == EvidencePurpose::HeldOutValidation && config.surface_plan.is_none() {
            return Err(CalibrationWorkerError::InvalidConfiguration);
        }

        let manifest_sha256 = config
            .manifest
            .sha256()
            .map_err(CalibrationWorkerError::Manifest)?;
        let transport =
            DeviceTransport::new(config.manifest.frame_identities()?, config.transport)?;
        let keyboard_filter = KeyboardLabelFilter::new(config.keyboard_source.device_id.clone());
        Ok(Self {
            manifest: config.manifest,
            manifest_sha256,
            keyboard_source: config.keyboard_source,
            keyboard_layout_confirmation: None,
            keyboard_filter,
            purpose: config.purpose,
            inherited_matching_rule: config.matching_rule,
            session_id: config.session_id,
            created_unix_ns: config.created_unix_ns,
            started_host_time_ns: config.started_host_time_ns,
            surface_plan: config.surface_plan,
            transport,
            connected: [true, true],
            last_sensor_receipt_ns: [None, None],
            frame_integrity: std::array::from_fn(|_| FrameIntegrity::default()),
            records: Vec::new(),
            retained_record_payload_bytes: 0,
            valid_frame_counts: [0, 0],
            events: Vec::new(),
            neutral_windows: Vec::new(),
            last_host_time_ns: config.started_host_time_ns,
            terminal_fault: None,
            label_admission_open: true,
            frozen: false,
            retention_complete: true,
            close_errors: Vec::new(),
        })
    }

    /// Preserve one source-tagged byte, tick, or disconnect event.
    pub fn handle_sensor(
        &mut self,
        event: DeviceTransportEvent,
    ) -> Result<(), CalibrationWorkerError> {
        self.require_healthy()?;
        if self.frozen {
            return Err(CalibrationWorkerError::LabelAdmissionClosed);
        }
        let (event_time_ns, disconnected, received_source) = match &event {
            DeviceTransportEvent::Bytes {
                source,
                receipt_time_ns,
                ..
            } => (*receipt_time_ns, None, Some(*source)),
            DeviceTransportEvent::Disconnected {
                source,
                receipt_time_ns,
            } => (*receipt_time_ns, Some(*source), None),
            DeviceTransportEvent::Tick { host_time_ns } => (*host_time_ns, None, None),
        };
        self.last_host_time_ns = self.last_host_time_ns.max(event_time_ns);
        let mut candidate_fault = None;
        let integrity = &mut self.frame_integrity;
        let update = self.transport.handle_observed(&event, |candidate| {
            if candidate_fault.is_some() {
                return;
            }
            let source = candidate.source;
            let source_ordinal = candidate.source_ordinal;
            let frame = candidate
                .frame
                .as_ref()
                .and_then(|frame| frame.as_ref().ok());
            candidate_fault = match (candidate.identity, frame) {
                (CandidateIdentity::Matched, Some(frame)) => integrity[hand_index(source)]
                    .observe(frame)
                    .err()
                    .map(|error| match error {
                        FrameIntegrityError::Quality(reason) => {
                            CalibrationWorkerFault::FrameQuality {
                                source,
                                source_ordinal,
                                reason,
                            }
                        }
                        FrameIntegrityError::Structure(error) => {
                            CalibrationWorkerFault::FrameStructure {
                                source,
                                source_ordinal,
                                error,
                            }
                        }
                    }),
                (CandidateIdentity::Mismatch { expected, actual }, _) => Some(
                    CalibrationWorkerFault::Transport(DeviceTransportFault::IdentityMismatch {
                        source,
                        expected,
                        actual,
                        receipt_time_ns: candidate.receipt_time_ns,
                        source_ordinal,
                    }),
                ),
                _ => Some(CalibrationWorkerFault::InvalidCandidate {
                    source,
                    source_ordinal,
                }),
            };
        });
        let update = match update {
            Ok(update) => update,
            Err(error) => {
                self.fail(CalibrationWorkerFault::TransportAdmission(error));
                match self.transport.retain_evidence(event) {
                    Ok(update) => {
                        if let Some(source) = disconnected {
                            self.connected[hand_index(source)] = false;
                        }
                        self.retain_update(update, true);
                    }
                    Err(retention_error) => {
                        self.retention_complete = false;
                        self.close_errors.push(retention_error.to_string());
                    }
                }
                self.disconnect_sources(event_time_ns, true);
                return self.require_healthy();
            }
        };
        if let Some(fault) = candidate_fault {
            self.fail(fault);
        }
        if let Some(source) = received_source {
            let receipt = &mut self.last_sensor_receipt_ns[hand_index(source)];
            *receipt = Some(receipt.map_or(event_time_ns, |previous| previous.max(event_time_ns)));
        }
        if let Some(source) = disconnected {
            self.connected[hand_index(source)] = false;
        }
        self.retain_update(update, false);
        if self.terminal_fault.is_some() {
            // A transport fault can be reported before the merge watermark
            // releases its offending candidate. Close both sources now so the
            // worker retains every buffered or unterminated record before it
            // becomes terminal.
            self.disconnect_sources(event_time_ns, true);
        }
        self.require_healthy()
    }

    /// Preserve and classify one event from the exact selected keyboard.
    pub fn handle_keyboard(
        &mut self,
        source: &KeyboardSourceIdentity,
        event: RawKeyboardEvent,
        context: KeyboardCaptureContext,
    ) -> Result<LabelDisposition, CalibrationWorkerError> {
        self.require_healthy()?;
        self.require_label_admission()?;
        if self.purpose == EvidencePurpose::HeldOutValidation && context.include_as_candidate {
            return Err(CalibrationWorkerError::KeyboardNotAllowed);
        }
        validate_keyboard_context(self.purpose, context)?;
        if source != &self.keyboard_source || event.device_id != self.keyboard_source.device_id {
            self.fail(CalibrationWorkerFault::KeyboardSourceChanged);
            return Err(CalibrationWorkerError::Faulted(
                CalibrationWorkerFault::KeyboardSourceChanged,
            ));
        }
        self.last_host_time_ns = self.last_host_time_ns.max(event.host_time_ns);
        self.require_event_capacity()?;
        let disposition = self.keyboard_filter.process(event.clone());
        self.events.push(if context.include_as_candidate {
            CapturedCalibrationEvent::Keyboard {
                role: context.role,
                domain: context.domain,
                block_id: context.block_id,
                event,
            }
        } else {
            CapturedCalibrationEvent::KeyboardStateOnly {
                role: context.role,
                domain: context.domain,
                block_id: context.block_id,
                event,
            }
        });
        Ok(disposition)
    }

    /// Record one acknowledged plan-owned browser presentation.
    pub fn record_display_acknowledgement(
        &mut self,
        prompt_id: PromptId,
        presentation: SurfacePresentationReceipt,
    ) -> Result<(), CalibrationWorkerError> {
        self.require_healthy()?;
        self.require_label_admission()?;
        self.require_event_capacity()?;
        self.last_host_time_ns = self
            .last_host_time_ns
            .max(presentation.acknowledged_host_time_ns());
        self.events
            .push(CapturedCalibrationEvent::DisplayAcknowledged {
                prompt_id,
                presentation,
            });
        Ok(())
    }

    /// Record one service-scheduled null sample as facts owned by its plan ID.
    pub fn record_idle(
        &mut self,
        prompt_id: PromptId,
        sample_ordinal: u32,
        trigger_host_time_ns: u64,
    ) -> Result<(), CalibrationWorkerError> {
        self.require_healthy()?;
        self.require_label_admission()?;
        self.require_event_capacity()?;
        self.last_host_time_ns = self.last_host_time_ns.max(trigger_host_time_ns);
        self.events.push(CapturedCalibrationEvent::Idle {
            prompt_id,
            sample_ordinal,
            trigger_host_time_ns,
        });
        Ok(())
    }

    /// Bind the exact immutable surface plan before the first surface fact.
    pub fn bind_surface_plan(
        &mut self,
        binding: CalibrationSurfacePlanBinding,
    ) -> Result<(), CalibrationWorkerError> {
        self.require_healthy()?;
        binding
            .validate(self.purpose)
            .map_err(CalibrationWorkerError::Capture)?;
        if self.events.iter().any(|event| {
            matches!(
                event,
                CapturedCalibrationEvent::DisplayAcknowledged { .. }
                    | CapturedCalibrationEvent::Idle { .. }
            )
        }) {
            return Err(CalibrationWorkerError::InvalidConfiguration);
        }
        match &self.surface_plan {
            Some(existing) if existing == &binding => Ok(()),
            Some(_) => Err(CalibrationWorkerError::InvalidConfiguration),
            None => {
                self.surface_plan = Some(binding);
                Ok(())
            }
        }
    }

    /// Bind fitting neutral ranges to record ordinals already retained.
    pub fn set_neutral_windows(
        &mut self,
        windows: Vec<NeutralRecordWindow>,
    ) -> Result<(), CalibrationWorkerError> {
        self.require_healthy()?;
        if self.purpose != EvidencePurpose::Fitting
            || windows.is_empty()
            || windows
                .last()
                .is_some_and(|window| window.end_record_exclusive > self.record_count())
        {
            return Err(CalibrationWorkerError::InvalidNeutralWindows);
        }
        self.neutral_windows = windows;
        Ok(())
    }

    /// Freeze the neutral-derived contact rule before the first surface cue.
    ///
    /// A later fitting checkpoint recomputes its thresholds from the retained
    /// neutral source, so installing a different rule cannot change evidence.
    pub fn freeze_contact_rule(
        &mut self,
        rule: CalibrationMatchingRule,
    ) -> Result<(), CalibrationWorkerError> {
        self.require_healthy()?;
        if self.purpose != EvidencePurpose::Fitting {
            return Err(CalibrationWorkerError::InvalidMatchingRuleUse);
        }
        rule.validate().map_err(CalibrationWorkerError::Capture)?;
        if rule.acquisition_manifest_sha256 != format_sha256(&self.manifest_sha256) {
            return Err(CalibrationWorkerError::InvalidMatchingRuleUse);
        }
        match &self.inherited_matching_rule {
            Some(existing) if existing == &rule => Ok(()),
            Some(_) => Err(CalibrationWorkerError::InvalidMatchingRuleUse),
            None => {
                self.inherited_matching_rule = Some(rule);
                Ok(())
            }
        }
    }

    #[must_use]
    pub fn record_count(&self) -> u64 {
        u64::try_from(self.records.len()).unwrap_or(u64::MAX)
    }

    /// Number of exact identity-matched frames retained for one source.
    #[must_use]
    pub const fn valid_frame_count(&self, hand: Hand) -> u64 {
        self.valid_frame_counts[hand_index(hand)]
    }

    /// Latest host receipt time observed from both sensor reader streams.
    ///
    /// A caller can checkpoint a causal drain horizon only after this value
    /// reaches the horizon. Host-time ticks do not advance it because they do
    /// not prove that queued reader input has reached the worker.
    #[must_use]
    pub fn sensor_receipt_horizon_ns(&self) -> Option<u64> {
        match self.last_sensor_receipt_ns {
            [Some(left), Some(right)] => Some(left.min(right)),
            _ => None,
        }
    }

    #[must_use]
    pub const fn keyboard_identified(&self) -> bool {
        self.keyboard_filter.is_identified()
    }

    /// Bind the exact browser layout receipt before any evidence checkpoint.
    pub fn confirm_keyboard_layout(
        &mut self,
        receipt: KeyboardLayoutConfirmationReceipt,
    ) -> Result<(), CalibrationWorkerError> {
        if self.purpose == EvidencePurpose::Fitting && !self.keyboard_filter.is_identified() {
            return Err(CalibrationWorkerError::KeyboardLayoutNotConfirmed);
        }
        receipt
            .validate()
            .map_err(|_| CalibrationWorkerError::InvalidKeyboardLayoutConfirmation)?;
        match &self.keyboard_layout_confirmation {
            Some(existing) if existing == &receipt => Ok(()),
            Some(_) => Err(CalibrationWorkerError::ConflictingKeyboardLayoutConfirmation),
            None => {
                self.keyboard_layout_confirmation = Some(receipt);
                Ok(())
            }
        }
    }

    #[must_use]
    pub fn held_key_count(&self) -> usize {
        self.keyboard_filter.held_count()
    }

    /// Permanently stop all keyboard, display-acknowledgement, and scheduled-null labels.
    /// Sensor bytes may still be drained into a terminal incident.
    pub fn close_label_admission(&mut self) {
        self.label_admission_open = false;
    }

    /// Derive a neutral report from the released prefix without ending capture.
    pub fn derive_neutral_report(
        &self,
        windows: &[NeutralRecordWindow],
    ) -> Result<NeutralEvidenceReport, CalibrationWorkerError> {
        self.require_healthy()?;
        if self.purpose != EvidencePurpose::Fitting
            || windows.is_empty()
            || windows
                .last()
                .is_some_and(|window| window.end_record_exclusive > self.record_count())
        {
            return Err(CalibrationWorkerError::InvalidNeutralWindows);
        }
        let session = self.assemble_session()?;
        let clocks = ClockEpochSet::fit_qualified_session(&self.manifest, &session)?;
        NeutralEvidenceReport::derive_qualified(&clocks, windows)
            .map_err(CalibrationWorkerError::Neutral)
    }

    /// Build a verified immutable prefix while the physical streams stay open.
    ///
    /// Callers must advance the transport watermark first. Candidates still
    /// inside the bounded merge horizon are intentionally absent.
    pub fn checkpoint(&self) -> Result<CalibrationCaptureBundle, CalibrationWorkerError> {
        self.require_healthy()?;
        self.assemble_bundle(false)
    }

    /// Derive exact physical-keyboard coverage without consuming the worker.
    ///
    /// A clean prefix with no causal training match has exact zero coverage.
    /// Stream-integrity and clock errors remain terminal and are never
    /// converted into prompt deficits.
    pub fn keyboard_evidence_checkpoint(
        &self,
    ) -> Result<KeyboardEvidenceCheckpoint, CalibrationWorkerError> {
        self.require_healthy()?;
        let session = self.assemble_session()?;
        let (clocks, journal) = self.assemble_capture_inputs(&session)?;
        match_calibration_capture_qualified(&clocks, &journal)
            .map(|matched| {
                KeyboardEvidenceCheckpoint::from_observations(
                    matched
                        .observations()
                        .iter()
                        .filter(|observation| {
                            observation.role == EvidenceRole::Training
                                && observation.domain == EvidenceDomain::Keyboard
                        })
                        .filter_map(|observation| {
                            observation
                                .position
                                .map(|position| MatchedKeyboardObservation {
                                    position,
                                    block_id: observation.block_id,
                                    window_end_record: observation.window_end_record,
                                })
                        }),
                )
            })
            .map_err(Into::into)
    }

    /// Finalize raw evidence, fit clocks, build labels, and derive neutral data.
    pub fn finish(
        mut self,
        finished_host_time_ns: u64,
    ) -> Result<CalibrationCaptureBundle, CalibrationWorkerError> {
        self.freeze(finished_host_time_ns)?;
        self.derive_frozen()
    }

    /// Move the live stream into a fresh worker for the same locked source pair.
    /// Stage boundaries preserve sequence/sample-time history, pending
    /// candidates, and partial COBS frames. Released records stay here.
    pub(crate) fn transfer_transport_to(
        &mut self,
        next: &mut Self,
    ) -> Result<(), CalibrationWorkerError> {
        self.require_healthy()?;
        next.require_healthy()?;
        std::mem::swap(&mut self.transport, &mut next.transport);
        std::mem::swap(&mut self.connected, &mut next.connected);
        std::mem::swap(
            &mut self.last_sensor_receipt_ns,
            &mut next.last_sensor_receipt_ns,
        );
        std::mem::swap(&mut self.frame_integrity, &mut next.frame_integrity);
        Ok(())
    }

    /// Close the original decoder and labels before any background derivation.
    pub(crate) fn freeze(
        &mut self,
        finished_host_time_ns: u64,
    ) -> Result<(), CalibrationWorkerError> {
        if !self.frozen {
            self.close_label_admission();
            self.last_host_time_ns = self.last_host_time_ns.max(finished_host_time_ns);
            self.disconnect_sources(self.last_host_time_ns, false);
            self.frozen = true;
        }
        self.require_healthy()
    }

    /// Derive evidence while the runner retains independent ownership of input.
    pub(crate) fn derive_frozen(&self) -> Result<CalibrationCaptureBundle, CalibrationWorkerError> {
        self.require_frozen()?;
        self.require_healthy()?;
        if self.records.is_empty() || self.events.is_empty() {
            return Err(CalibrationWorkerError::IncompleteCapture);
        }
        if self.purpose == EvidencePurpose::Fitting && self.neutral_windows.is_empty() {
            return Err(CalibrationWorkerError::InvalidNeutralWindows);
        }
        self.assemble_bundle(true)
    }

    /// Recover diagnostic input even when derivation fails or its receiver closes.
    pub(crate) fn frozen_incident(
        &self,
        reason: CalibrationIncidentReason,
    ) -> Result<CalibrationCaptureIncident, CalibrationWorkerError> {
        self.require_frozen()?;
        self.assemble_incident(reason, self.events.clone(), self.close_errors.clone())
    }

    /// Close retained input as diagnostic incident material without deriving
    /// clocks, matching labels, evidence, an artifact, or a validation report.
    pub fn close_incident(
        mut self,
        finished_host_time_ns: u64,
        reason: CalibrationIncidentReason,
    ) -> Result<CalibrationCaptureIncident, CalibrationWorkerError> {
        if !self.frozen {
            self.close_label_admission();
            self.last_host_time_ns = self.last_host_time_ns.max(finished_host_time_ns);
            self.disconnect_sources(self.last_host_time_ns, true);
        }
        let events = std::mem::take(&mut self.events);
        let close_errors = std::mem::take(&mut self.close_errors);
        self.assemble_incident(reason, events, close_errors)
    }

    fn assemble_incident(
        &self,
        reason: CalibrationIncidentReason,
        events: Vec<CapturedCalibrationEvent>,
        close_errors: Vec<String>,
    ) -> Result<CalibrationCaptureIncident, CalibrationWorkerError> {
        let session = build_closed_session(
            self.manifest.nominal_period_us,
            self.manifest_sha256,
            self.session_id,
            &self.records,
        )?;
        let retained_records = session.summary().total_records;
        let retained_events = u64::try_from(events.len()).unwrap_or(u64::MAX);
        let journal = CalibrationIncidentJournal {
            schema: CALIBRATION_INCIDENT_SCHEMA.to_owned(),
            evidence_eligible: false,
            reason,
            purpose: self.purpose,
            acquisition_manifest_sha256: format_sha256(&self.manifest_sha256),
            session_sha256: format_sha256(&sha256(session.bytes())),
            session_id: self.session_id,
            created_unix_ns: self.created_unix_ns,
            closed_host_time_ns: self.last_host_time_ns,
            retained_records,
            retained_events,
            source_close_complete: self.connected.iter().all(|connected| !connected),
            retention_complete: self.retention_complete,
            terminal_fault: self.terminal_fault.map(|fault| fault.to_string()),
            close_errors,
            events,
        };
        let canonical_journal = journal.canonical_json()?;
        Ok(CalibrationCaptureIncident {
            session,
            journal,
            canonical_journal,
        })
    }

    fn assemble_bundle(
        &self,
        require_complete_surface_plan: bool,
    ) -> Result<CalibrationCaptureBundle, CalibrationWorkerError> {
        let session = self.assemble_session()?;
        let (clocks, journal) = self.assemble_capture_inputs(&session)?;
        if require_complete_surface_plan {
            journal
                .validate_complete_surface_plan()
                .map_err(CalibrationWorkerError::Capture)?;
        }
        let (evidence, capture_report) = build_calibration_evidence_qualified(&clocks, &journal)?;
        let neutral_report = if self.purpose == EvidencePurpose::Fitting {
            Some(NeutralEvidenceReport::derive_qualified(
                &clocks,
                &self.neutral_windows,
            )?)
        } else {
            None
        };
        let clocks = clocks.into_epochs();
        Ok(CalibrationCaptureBundle {
            session,
            clocks,
            journal,
            evidence,
            capture_report,
            neutral_report,
        })
    }

    fn assemble_session(&self) -> Result<FinalizedSession, CalibrationWorkerError> {
        build_session(
            self.manifest.nominal_period_us,
            self.manifest_sha256,
            self.session_id,
            &self.records,
        )
        .map_err(Into::into)
    }

    fn assemble_capture_inputs<'a>(
        &'a self,
        session: &'a FinalizedSession,
    ) -> Result<(QualifiedOfflineClocks<'a>, CalibrationCaptureJournal), CalibrationWorkerError>
    {
        let setup_seconds = self
            .last_host_time_ns
            .saturating_sub(self.started_host_time_ns)
            .div_ceil(1_000_000_000)
            .max(1);
        let setup_seconds =
            u32::try_from(setup_seconds).map_err(|_| CalibrationWorkerError::SetupTimeExceeded)?;
        if setup_seconds > crate::calibration::SETUP_TIME_CAP_SECONDS {
            return Err(CalibrationWorkerError::SetupTimeExceeded);
        }

        let clocks = ClockEpochSet::fit_qualified_session(&self.manifest, session)?;
        let journal = CalibrationCaptureJournal::new(CalibrationCaptureInput {
            purpose: self.purpose,
            acquisition_manifest_sha256: self.manifest_sha256,
            clock_epochs_sha256: clocks
                .epochs()
                .fingerprint()
                .map_err(CalibrationWorkerError::Clock)?,
            session_sha256: session.sha256(),
            session_id: self.session_id,
            keyboard_device_id: self.keyboard_source.device_id.clone(),
            keyboard: self.keyboard_source.artifact_identity(
                self.keyboard_layout_confirmation
                    .clone()
                    .ok_or(CalibrationWorkerError::KeyboardLayoutNotConfirmed)?,
            ),
            setup_seconds,
            created_unix_ns: self.created_unix_ns,
            pre_rejected_examples: 0,
            neutral_windows: self.neutral_windows.clone(),
            matching_rule: self.inherited_matching_rule.clone(),
            surface_plan: self.surface_plan.clone(),
            events: self.events.clone(),
        })?;
        Ok((clocks, journal))
    }

    fn retain_update(&mut self, update: DeviceTransportUpdate, finishing: bool) {
        for candidate in update.candidates {
            let payload_bytes = candidate_payload_len(&candidate);
            if self.records.len() >= MAX_RETAINED_RECORDS
                || self
                    .retained_record_payload_bytes
                    .saturating_add(payload_bytes)
                    > MAX_RETAINED_RECORD_PAYLOAD_BYTES
            {
                self.retention_complete = false;
                self.fail(CalibrationWorkerFault::RetentionLimitExceeded);
                continue;
            }
            let source = candidate.source;
            let source_ordinal = candidate.source_ordinal;
            // Keep the public identity-matched retention count even during
            // terminal drainage. Worker health separately gates evidence.
            let valid = candidate.identity == CandidateIdentity::Matched
                && matches!(&candidate.candidate, TransportCandidate::Decoded(_));
            if valid {
                self.valid_frame_counts[hand_index(source)] =
                    self.valid_frame_counts[hand_index(source)].saturating_add(1);
            }
            self.retained_record_payload_bytes = self
                .retained_record_payload_bytes
                .saturating_add(payload_bytes);
            self.records.push(candidate);
            if !valid {
                self.fail(CalibrationWorkerFault::InvalidCandidate {
                    source,
                    source_ordinal,
                });
            }
        }
        for fault in update.faults {
            if finishing && matches!(fault, DeviceTransportFault::SourceDisconnected { .. }) {
                continue;
            }
            self.fail(CalibrationWorkerFault::Transport(fault));
        }
    }

    fn disconnect_sources(&mut self, receipt_time_ns: u64, evidence_only: bool) {
        for source in [Hand::Left, Hand::Right] {
            if !self.connected[hand_index(source)] {
                continue;
            }
            let event = DeviceTransportEvent::Disconnected {
                source,
                receipt_time_ns,
            };
            // Healthy capture keeps the operational receipt merge order. A
            // terminal closure must retain bytes even when that merge rejects.
            let update = if evidence_only || self.terminal_fault.is_some() {
                self.transport.retain_evidence(event)
            } else {
                match self.transport.handle(&event) {
                    Ok(update) => Ok(update),
                    Err(error) => {
                        self.fail(CalibrationWorkerFault::TransportAdmission(error));
                        self.transport.retain_evidence(event)
                    }
                }
            };
            match update {
                Ok(update) => {
                    self.connected[hand_index(source)] = false;
                    self.retain_update(update, true);
                }
                Err(error) => {
                    self.retention_complete = false;
                    self.close_errors.push(error.to_string());
                    self.fail(CalibrationWorkerFault::TransportAdmission(error));
                }
            }
        }
    }

    fn fail(&mut self, fault: CalibrationWorkerFault) {
        if self.terminal_fault.is_none() {
            self.terminal_fault = Some(fault);
            self.close_label_admission();
        }
    }

    pub(crate) fn require_healthy(&self) -> Result<(), CalibrationWorkerError> {
        match self.terminal_fault {
            Some(CalibrationWorkerFault::TransportAdmission(error)) => {
                Err(CalibrationWorkerError::Transport(error))
            }
            Some(fault) => Err(CalibrationWorkerError::Faulted(fault)),
            None => Ok(()),
        }
    }

    fn require_frozen(&self) -> Result<(), CalibrationWorkerError> {
        if self.frozen {
            Ok(())
        } else {
            Err(CalibrationWorkerError::CaptureNotFrozen)
        }
    }

    fn require_label_admission(&self) -> Result<(), CalibrationWorkerError> {
        if self.label_admission_open {
            Ok(())
        } else {
            Err(CalibrationWorkerError::LabelAdmissionClosed)
        }
    }

    fn require_event_capacity(&mut self) -> Result<(), CalibrationWorkerError> {
        if self.events.len() < MAX_RETAINED_EVENTS {
            return Ok(());
        }
        self.retention_complete = false;
        self.fail(CalibrationWorkerFault::RetentionLimitExceeded);
        Err(CalibrationWorkerError::Faulted(
            CalibrationWorkerFault::RetentionLimitExceeded,
        ))
    }
}

fn validate_keyboard_context(
    purpose: EvidencePurpose,
    context: KeyboardCaptureContext,
) -> Result<(), CalibrationWorkerError> {
    let expected_role = match purpose {
        EvidencePurpose::Fitting => EvidenceRole::Training,
        EvidencePurpose::HeldOutValidation => EvidenceRole::HeldOutValidation,
    };
    if context.block_id == 0
        || context.domain != EvidenceDomain::Keyboard
        || context.role != expected_role
        || (purpose == EvidencePurpose::HeldOutValidation && context.include_as_candidate)
    {
        return Err(CalibrationWorkerError::InvalidCaptureContext);
    }
    Ok(())
}

fn build_session(
    nominal_period_us: u32,
    manifest_sha256: [u8; 32],
    session_id: [u8; 16],
    records: &[SourcedTransportCandidate],
) -> Result<FinalizedSession, SessionError> {
    build_closed_session(nominal_period_us, manifest_sha256, session_id, records)?.into_admitted()
}

fn build_closed_session(
    nominal_period_us: u32,
    manifest_sha256: [u8; 32],
    session_id: [u8; 16],
    records: &[SourcedTransportCandidate],
) -> Result<ClosedSession, SessionError> {
    let mut writer = SessionWriter::new(
        Vec::new(),
        SessionHeader {
            nominal_period_us,
            manifest_sha256,
            session_id,
        },
    )?;
    for record in records {
        writer.append_transport_candidate(
            ControllerStream::from(record.source),
            &record.candidate,
            record.receipt_time_ns,
        )?;
    }
    writer.finish_closed()
}

fn candidate_payload_len(candidate: &SourcedTransportCandidate) -> usize {
    match &candidate.candidate {
        TransportCandidate::Decoded(payload) => payload.len(),
        TransportCandidate::Malformed { encoded }
        | TransportCandidate::Unterminated { encoded } => encoded.len(),
        TransportCandidate::Oversized { prefix, .. } => prefix.len(),
    }
}

const fn hand_index(hand: Hand) -> usize {
    match hand {
        Hand::Left => 0,
        Hand::Right => 1,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CalibrationWorkerFault {
    Transport(DeviceTransportFault),
    TransportAdmission(DeviceTransportError),
    InvalidCandidate {
        source: Hand,
        source_ordinal: u64,
    },
    FrameQuality {
        source: Hand,
        source_ordinal: u64,
        reason: AbstainReason,
    },
    FrameStructure {
        source: Hand,
        source_ordinal: u64,
        error: WindowError,
    },
    KeyboardSourceChanged,
    RetentionLimitExceeded,
}

impl fmt::Display for CalibrationWorkerFault {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(fault) => write!(formatter, "controller transport fault: {fault:?}"),
            Self::TransportAdmission(error) => {
                write!(formatter, "controller transport admission failed: {error}")
            }
            Self::InvalidCandidate {
                source,
                source_ordinal,
            } => write!(
                formatter,
                "{source:?} controller candidate {source_ordinal} is not one valid identity-matched frame"
            ),
            Self::FrameQuality {
                source,
                source_ordinal,
                reason,
            } => write!(
                formatter,
                "{source:?} controller candidate {source_ordinal} violates acquisition integrity: {reason:?}"
            ),
            Self::FrameStructure {
                source,
                source_ordinal,
                error,
            } => write!(
                formatter,
                "{source:?} controller candidate {source_ordinal} violates stream continuity: {error}"
            ),
            Self::KeyboardSourceChanged => {
                formatter.write_str("selected physical keyboard identity changed")
            }
            Self::RetentionLimitExceeded => {
                formatter.write_str("bounded calibration incident retention limit was exceeded")
            }
        }
    }
}

#[derive(Debug)]
pub enum CalibrationWorkerError {
    InvalidConfiguration,
    InvalidMatchingRuleUse,
    InvalidCaptureContext,
    InvalidNeutralWindows,
    KeyboardLayoutNotConfirmed,
    InvalidKeyboardLayoutConfirmation,
    ConflictingKeyboardLayoutConfirmation,
    KeyboardNotAllowed,
    IncompleteCapture,
    LabelAdmissionClosed,
    CaptureNotFrozen,
    SetupTimeExceeded,
    IncidentTooLarge { actual: usize },
    IncidentSerialization(String),
    Manifest(ManifestError),
    TransportConfiguration(DeviceTransportConfigurationError),
    Transport(DeviceTransportError),
    Session(SessionError),
    ClockFit(ClockEpochFitError),
    Clock(crate::clock_epoch::ClockEpochError),
    Capture(CalibrationCaptureError),
    Neutral(NeutralEvidenceError),
    Faulted(CalibrationWorkerFault),
}

impl fmt::Display for CalibrationWorkerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfiguration => {
                formatter.write_str("calibration worker configuration is incomplete")
            }
            Self::InvalidMatchingRuleUse => formatter.write_str(
                "fitting must learn one matching rule and held-out capture must inherit it",
            ),
            Self::InvalidCaptureContext => {
                formatter.write_str("calibration event role, domain, or block is invalid")
            }
            Self::InvalidNeutralWindows => {
                formatter.write_str("fitting capture has no valid retained neutral window")
            }
            Self::KeyboardLayoutNotConfirmed => {
                formatter.write_str("keyboard layout has no explicit confirmation receipt")
            }
            Self::InvalidKeyboardLayoutConfirmation => {
                formatter.write_str("keyboard layout confirmation receipt is invalid")
            }
            Self::ConflictingKeyboardLayoutConfirmation => {
                formatter.write_str("keyboard layout confirmation cannot change during one capture")
            }
            Self::KeyboardNotAllowed => {
                formatter.write_str("held-out capture cannot admit physical-keyboard candidates")
            }
            Self::IncompleteCapture => {
                formatter.write_str("calibration capture has no complete sensor or label evidence")
            }
            Self::LabelAdmissionClosed => {
                formatter.write_str("calibration label admission is permanently closed")
            }
            Self::CaptureNotFrozen => formatter
                .write_str("calibration input must be frozen before derivation or retention"),
            Self::SetupTimeExceeded => {
                formatter.write_str("calibration capture exceeded the finite setup time cap")
            }
            Self::IncidentTooLarge { actual } => write!(
                formatter,
                "calibration incident is {actual} bytes; maximum is {MAX_CALIBRATION_INCIDENT_BYTES}"
            ),
            Self::IncidentSerialization(error) => {
                write!(
                    formatter,
                    "calibration incident serialization failed: {error}"
                )
            }
            Self::Manifest(error) => write!(formatter, "calibration manifest failed: {error}"),
            Self::TransportConfiguration(error) => {
                write!(
                    formatter,
                    "calibration transport configuration failed: {error}"
                )
            }
            Self::Transport(error) => write!(formatter, "calibration transport failed: {error}"),
            Self::Session(error) => write!(formatter, "calibration session failed: {error}"),
            Self::ClockFit(error) => write!(formatter, "calibration clock fit failed: {error}"),
            Self::Clock(error) => write!(formatter, "calibration clock failed: {error}"),
            Self::Capture(error) => write!(formatter, "calibration journal failed: {error}"),
            Self::Neutral(error) => {
                write!(formatter, "calibration neutral evidence failed: {error}")
            }
            Self::Faulted(error) => write!(formatter, "calibration capture stopped: {error}"),
        }
    }
}

impl Error for CalibrationWorkerError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Manifest(error) => Some(error),
            Self::TransportConfiguration(error) => Some(error),
            Self::Transport(error) => Some(error),
            Self::Session(error) => Some(error),
            Self::ClockFit(error) => Some(error),
            Self::Clock(error) => Some(error),
            Self::Capture(error) => Some(error),
            Self::Neutral(error) => Some(error),
            _ => None,
        }
    }
}

impl From<ManifestError> for CalibrationWorkerError {
    fn from(error: ManifestError) -> Self {
        Self::Manifest(error)
    }
}

impl From<DeviceTransportConfigurationError> for CalibrationWorkerError {
    fn from(error: DeviceTransportConfigurationError) -> Self {
        Self::TransportConfiguration(error)
    }
}

impl From<DeviceTransportError> for CalibrationWorkerError {
    fn from(error: DeviceTransportError) -> Self {
        Self::Transport(error)
    }
}

impl From<SessionError> for CalibrationWorkerError {
    fn from(error: SessionError) -> Self {
        Self::Session(error)
    }
}

impl From<ClockEpochFitError> for CalibrationWorkerError {
    fn from(error: ClockEpochFitError) -> Self {
        Self::ClockFit(error)
    }
}

impl From<CalibrationCaptureError> for CalibrationWorkerError {
    fn from(error: CalibrationCaptureError) -> Self {
        Self::Capture(error)
    }
}

impl From<NeutralEvidenceError> for CalibrationWorkerError {
    fn from(error: NeutralEvidenceError) -> Self {
        Self::Neutral(error)
    }
}
