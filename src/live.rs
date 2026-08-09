//! Causal, user-attested two-hand runtime with terminal fault revocation.

use std::{
    error::Error,
    fmt,
    sync::atomic::{AtomicU64, Ordering},
};

use crate::{
    artifact::CalibrationArtifact,
    clock::{ClockError, ClockFit, OnlineClockDiagnostics, OnlineClockSynchronizer},
    decoder::{AbstainReason, DecisionKind, FrameIntegrity, FrameIntegrityError},
    device_transport::{
        CandidateIdentity, DeviceTransport, DeviceTransportError, DeviceTransportEvent,
        DeviceTransportUpdate, SourcedTransportCandidate,
    },
    layout::Hand,
    manifest::{AcquisitionManifest, FrameCanonicalizationError, FrameCanonicalizer},
    output::{FaultReason, OutputAdapter, OutputGateError, ShutdownSignal},
    pipeline::PipelineError,
    protocol::{FrameIdentity, SensorFrame},
    replay::{
        CalibratedRuntimeFault, MAX_MAPPED_RECEIPT_DELTA_NS, MAX_MERGE_PENDING_RECORDS,
        RuntimeBindingError, verify_runtime_binding_hashes,
    },
    runtime::{
        LiveRuntime, RuntimeConfigurationError, RuntimeError, RuntimeOutputError,
        RuntimeStartError, RuntimeStep, TransportFault,
    },
    transport::TransportCandidate,
    validation::WearSessionConfirmation,
};

pub const LIVE_WATCHDOG_NS: u64 = 100_000_000;
pub const USER_ATTESTED_UNMOVED_STATUS: &str = "user-attested-unmoved";
pub const REVOKED_STATUS: &str = "revoked";

static NEXT_LIVE_OWNER: AtomicU64 = AtomicU64::new(1);

/// A receipt-merger update admitted by exactly one live authority.
/// Raw evidence must be appended before this update is consumed for output.
#[must_use = "append raw evidence before releasing or retaining this update"]
pub struct LiveTransportUpdate<E> {
    evidence: DeviceTransportUpdate,
    ingress_error: Option<LiveCalibratedError<E>>,
    owner: u64,
    upstream_watermark_ns: Option<u64>,
}

impl<E> LiveTransportUpdate<E> {
    #[must_use]
    pub const fn evidence(&self) -> &DeviceTransportUpdate {
        &self.evidence
    }

    pub fn take_ingress_error(&mut self) -> Option<LiveCalibratedError<E>> {
        self.ingress_error.take()
    }

    #[must_use]
    pub fn into_evidence(self) -> DeviceTransportUpdate {
        self.evidence
    }
}

struct PendingFrame {
    mapped_time_ns: u64,
    receipt_time_ns: u64,
    ordinal: u64,
    frame: SensorFrame,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LiveCalibratedReport {
    pub total_candidates: u64,
    pub decoded_frames: u64,
    pub left_frames: u64,
    pub right_frames: u64,
    pub clock_warmup_frames: u64,
    pub armed_transitions: u64,
    pub invalid_candidates: u64,
    pub reordered_frames: u64,
    pub decisions: u64,
    pub key_decisions: u64,
    pub abstentions: u64,
    pub output_events: u64,
    pub watchdog_timeouts: u64,
    pub left_watchdog_timeouts: u64,
    pub right_watchdog_timeouts: u64,
    pub maximum_pending_frames: usize,
    pub runtime_faults: Vec<CalibratedRuntimeFault>,
    pub terminal_reason: Option<LiveTerminalReason>,
    pub finished_cleanly: bool,
    pub final_key_state_neutral: bool,
    pub output_neutrality_confirmed: bool,
}

impl LiveCalibratedReport {
    /// The live authority is a user attestation, never a sensor-verified mount.
    #[must_use]
    pub const fn wear_session_status(&self) -> &'static str {
        if self.finished_cleanly || self.terminal_reason.is_some() {
            REVOKED_STATUS
        } else {
            USER_ATTESTED_UNMOVED_STATUS
        }
    }
}

/// First cause that permanently revoked one calibrated live instance.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LiveTerminalReason {
    SourceLost(Hand),
    Disconnected,
    WatchdogTimeout {
        left_expired: bool,
        right_expired: bool,
    },
    HostReceiptRegression {
        ordinal: u64,
    },
    HostClockRegression,
    MappedReceiptDeltaTooLarge {
        ordinal: u64,
        actual_ns: u64,
    },
    PendingCapacityExceeded {
        maximum: usize,
        required: usize,
    },
    FrameCanonicalization,
    ClockFailure(Hand),
    EvidenceInvalidatingAbstention(AbstainReason),
    RuntimeFault(CalibratedRuntimeFault),
    OutputGateFailure,
    OutputFailure,
    PlannedShutdown(ShutdownSignal),
}

pub struct LiveCalibratedExecution<A: OutputAdapter> {
    pub report: LiveCalibratedReport,
    pub output: A,
    pub neutralization_failure: Option<RuntimeOutputError<A::Error>>,
    pub left_clock_fit: Option<ClockFit>,
    pub right_clock_fit: Option<ClockFit>,
    pub left_clock_diagnostics: OnlineClockDiagnostics,
    pub right_clock_diagnostics: OnlineClockDiagnostics,
}

pub struct LiveCalibratedRuntime<A: OutputAdapter> {
    runtime: LiveRuntime<A>,
    owner: u64,
    canonicalizer: FrameCanonicalizer,
    left_clock: OnlineClockSynchronizer,
    right_clock: OnlineClockSynchronizer,
    left_identity: FrameIdentity,
    right_identity: FrameIdentity,
    frame_integrity: [FrameIntegrity; 2],
    pending: Vec<PendingFrame>,
    last_candidate_receipt_time_ns: Option<u64>,
    last_consumer_time_ns: Option<u64>,
    upstream_watermark_ns: Option<u64>,
    last_left_receipt_time_ns: Option<u64>,
    last_right_receipt_time_ns: Option<u64>,
    highest_processed_ordinal: Option<u64>,
    next_ordinal: u64,
    report: LiveCalibratedReport,
}

impl<A: OutputAdapter> LiveCalibratedRuntime<A> {
    pub fn new(
        calibration_manifest: &AcquisitionManifest,
        runtime_manifest: &AcquisitionManifest,
        artifact: &CalibrationArtifact,
        confirmation: WearSessionConfirmation,
        output: A,
    ) -> Result<Self, LiveCalibratedError<A::Error>> {
        verify_runtime_binding_hashes(
            calibration_manifest,
            runtime_manifest,
            artifact,
            confirmation.artifact_sha256(),
            confirmation.validation_report_sha256(),
        )
        .map_err(LiveCalibratedError::Binding)?;
        let canonicalizer = FrameCanonicalizer::new(runtime_manifest)
            .map_err(RuntimeBindingError::Manifest)
            .map_err(LiveCalibratedError::Binding)?;
        let left_identity = runtime_manifest.hands[0]
            .frame_identity()
            .expect("validated left wire identity exists");
        let right_identity = runtime_manifest.hands[1]
            .frame_identity()
            .expect("validated right wire identity exists");
        let left = artifact
            .decoder(Hand::Left)
            .expect("binding validated two-hand artifact")
            .clone();
        let right = artifact
            .decoder(Hand::Right)
            .expect("binding validated two-hand artifact")
            .clone();
        let runtime = LiveRuntime::two_hand(left, right, output).map_err(live_start_error)?;
        let output_neutrality_confirmed = runtime.output_neutrality_confirmed();
        Ok(Self {
            runtime,
            owner: NEXT_LIVE_OWNER.fetch_add(1, Ordering::Relaxed),
            canonicalizer,
            left_clock: OnlineClockSynchronizer::new(left_identity.boot_id),
            right_clock: OnlineClockSynchronizer::new(right_identity.boot_id),
            left_identity,
            right_identity,
            pending: Vec::new(),
            last_candidate_receipt_time_ns: None,
            last_consumer_time_ns: None,
            frame_integrity: Default::default(),
            upstream_watermark_ns: None,
            last_left_receipt_time_ns: None,
            last_right_receipt_time_ns: None,
            highest_processed_ordinal: None,
            next_ordinal: 0,
            report: LiveCalibratedReport {
                output_neutrality_confirmed,
                ..LiveCalibratedReport::default()
            },
        })
    }

    /// Admit completed candidates before the transport receipt merger can hold
    /// them. Integrity faults neutralize immediately, before any evidence I/O.
    /// Transport admission errors leave the event unconsumed.
    /// Use one fresh transport exclusively with one live runtime, including
    /// bootstrap backlog. Do not populate it through generic `handle` or move
    /// its admitted backlog to another live authority.
    /// `on_revoked` closes external input admission at the first integrity fault.
    pub fn admit_transport_event(
        &mut self,
        transport: &mut DeviceTransport,
        event: &DeviceTransportEvent,
        mut on_revoked: impl FnMut(),
    ) -> Result<LiveTransportUpdate<A::Error>, DeviceTransportError> {
        let mut ingress_error = self.ensure_active().err();
        let mut identity_fault_seen = false;
        let evidence = if ingress_error.is_some() {
            DeviceTransportUpdate::default()
        } else {
            transport.handle_observed(event, |candidate| {
                if ingress_error.is_some() || identity_fault_seen {
                    return;
                }
                if matches!(candidate.identity, CandidateIdentity::Mismatch { .. }) {
                    identity_fault_seen = true;
                    return;
                }
                let (terminal, neutral_reason) = if candidate.identity
                    == CandidateIdentity::Unavailable
                {
                    let (fault, neutral_reason) = invalid_candidate_fault(candidate);
                    self.report.total_candidates += 1;
                    self.next_ordinal = self.next_ordinal.saturating_add(1);
                    self.report.invalid_candidates += 1;
                    self.report.runtime_faults.push(fault.clone());
                    (LiveTerminalReason::RuntimeFault(fault), neutral_reason)
                } else {
                    let frame = candidate
                        .frame
                        .as_ref()
                        .and_then(|frame| frame.as_ref().ok())
                        .expect("matched identity has a decoded frame");
                    match self.frame_integrity[frame.hand as usize].observe(frame) {
                        Ok(()) => {
                            if let Err(error) = self.enforce_watchdog(candidate.receipt_time_ns) {
                                ingress_error = Some(error);
                                on_revoked();
                                return;
                            }
                            self.observe_hand_receipt(frame.hand, candidate.receipt_time_ns);
                            return;
                        }
                        Err(error) => {
                            let terminal = match error {
                                FrameIntegrityError::Quality(reason) => {
                                    LiveTerminalReason::EvidenceInvalidatingAbstention(reason)
                                }
                                FrameIntegrityError::Structure(error) => {
                                    let fault = CalibratedRuntimeFault::Pipeline(
                                        PipelineError::InvalidWindow(error),
                                    );
                                    self.report.runtime_faults.push(fault.clone());
                                    LiveTerminalReason::RuntimeFault(fault)
                                }
                            };
                            (terminal, FaultReason::DecoderReset)
                        }
                    }
                };
                self.terminate_neutral_preserving_primary(terminal.clone(), neutral_reason);
                ingress_error = Some(LiveCalibratedError::TerminalFault(terminal));
                on_revoked();
            })?
        };
        Ok(LiveTransportUpdate {
            evidence,
            ingress_error,
            owner: self.owner,
            upstream_watermark_ns: transport.watermark_ns(),
        })
    }

    /// Consume an admitted update after its raw evidence has been appended.
    /// Sample `consumer_time_ns` after that I/O, in the receipt clock's domain.
    /// The health gate precedes all candidate processing, including arming.
    /// The update cannot be cloned, replayed, or released by another runtime.
    pub fn release_transport_update(
        &mut self,
        update: LiveTransportUpdate<A::Error>,
        consumer_time_ns: u64,
    ) -> Result<(), LiveCalibratedError<A::Error>> {
        if update.owner != self.owner {
            return Err(LiveCalibratedError::ForeignAdmission);
        }
        if let Some(error) = update.ingress_error {
            return Err(error);
        }
        self.check_consumer_health(consumer_time_ns)?;
        for candidate in update.evidence.candidates {
            if !matches!(candidate.identity, CandidateIdentity::Mismatch { .. }) {
                self.process_candidate(candidate, consumer_time_ns)?;
            }
        }
        // Only the complete update establishes that no unreleased receipt is
        // behind this horizon. Tick uses the same consumer-time health gate.
        self.upstream_watermark_ns = self.upstream_watermark_ns.max(update.upstream_watermark_ns);
        Ok(())
    }

    fn process_candidate(
        &mut self,
        sourced: SourcedTransportCandidate,
        consumer_time_ns: u64,
    ) -> Result<(), LiveCalibratedError<A::Error>> {
        let receipt_time_ns = sourced.receipt_time_ns;
        self.ensure_active()?;
        if self
            .last_candidate_receipt_time_ns
            .is_some_and(|previous| receipt_time_ns < previous)
        {
            let reason = LiveTerminalReason::HostReceiptRegression {
                ordinal: self.next_ordinal,
            };
            self.terminate_neutral_preserving_primary(reason, FaultReason::DecoderReset);
            return Err(LiveCalibratedError::HostReceiptRegression {
                ordinal: self.next_ordinal,
            });
        }
        self.last_candidate_receipt_time_ns = Some(receipt_time_ns);
        self.report.total_candidates += 1;
        let ordinal = self.next_ordinal;
        self.next_ordinal = self.next_ordinal.saturating_add(1);

        let mut frame = sourced
            .frame
            .expect("live frame admission has a cached decode result")
            .expect("live frame admission rejected invalid candidates");
        frame = match self.canonicalizer.canonicalize(&frame) {
            Ok(frame) => frame,
            Err(error) => {
                self.terminate_neutral_preserving_primary(
                    LiveTerminalReason::FrameCanonicalization,
                    FaultReason::DecoderReset,
                );
                return Err(LiveCalibratedError::FrameCanonicalization(error));
            }
        };
        self.report.decoded_frames += 1;
        match frame.hand {
            Hand::Left => self.report.left_frames += 1,
            Hand::Right => self.report.right_frames += 1,
        }
        let mapped_time_ns = self.observe_clock(&frame, receipt_time_ns)?;
        let Some(mapped_time_ns) = mapped_time_ns else {
            self.report.clock_warmup_frames += 1;
            return Ok(());
        };
        // A frame whose operational mapping disagrees with its host receipt
        // permanently revokes this user-attested wear session.
        let receipt_delta_ns = mapped_time_ns.abs_diff(receipt_time_ns);
        if receipt_delta_ns > MAX_MAPPED_RECEIPT_DELTA_NS {
            self.terminate_neutral_preserving_primary(
                LiveTerminalReason::MappedReceiptDeltaTooLarge {
                    ordinal,
                    actual_ns: receipt_delta_ns,
                },
                FaultReason::DecoderReset,
            );
            return Err(LiveCalibratedError::MappedReceiptDeltaTooLarge {
                ordinal,
                actual_ns: receipt_delta_ns,
            });
        }
        if !self.clocks_ready() {
            self.report.clock_warmup_frames += 1;
            return Ok(());
        }
        if !self.runtime.is_armed() {
            if !self.check_consumer_health(consumer_time_ns)? {
                self.report.clock_warmup_frames += 1;
                return Ok(());
            }
            self.pending.clear();
            if let Err(error) = self.runtime.source_connected(Hand::Left) {
                return self.fail_output_gate(error);
            }
            if let Err(error) = self.runtime.source_connected(Hand::Right) {
                return self.fail_output_gate(error);
            }
            if let Err(error) = self.runtime.acquire_run_ownership() {
                return self.fail_output_gate(error);
            }
            self.report.armed_transitions += 1;
        }
        let watermark = receipt_time_ns.saturating_sub(MAX_MAPPED_RECEIPT_DELTA_NS);
        let pending = PendingFrame {
            mapped_time_ns,
            receipt_time_ns,
            ordinal,
            frame,
        };
        let required = self
            .pending
            .iter()
            .filter(|frame| frame.mapped_time_ns > watermark)
            .count()
            + usize::from(pending.mapped_time_ns > watermark);
        if required > MAX_MERGE_PENDING_RECORDS {
            let reason = LiveTerminalReason::PendingCapacityExceeded {
                maximum: MAX_MERGE_PENDING_RECORDS,
                required,
            };
            self.terminate_neutral_preserving_primary(reason.clone(), FaultReason::DecoderReset);
            return Err(LiveCalibratedError::TerminalFault(reason));
        }
        self.pending.push(pending);
        self.report.maximum_pending_frames =
            self.report.maximum_pending_frames.max(self.pending.len());
        self.flush(Some(watermark))
    }

    /// Advance the consumer's host clock after an input iteration, including
    /// candidate-free bytes. Enforce the watchdog before releasing merge backlog.
    pub fn tick(&mut self, host_time_ns: u64) -> Result<(), LiveCalibratedError<A::Error>> {
        if !self.check_consumer_health(host_time_ns)? {
            return Ok(());
        }
        if let Some(upstream_watermark_ns) = self.upstream_watermark_ns {
            self.flush(Some(
                upstream_watermark_ns.saturating_sub(MAX_MAPPED_RECEIPT_DELTA_NS),
            ))?;
        }
        Ok(())
    }

    fn check_consumer_health(
        &mut self,
        host_time_ns: u64,
    ) -> Result<bool, LiveCalibratedError<A::Error>> {
        self.ensure_active()?;
        // Raw receipts can precede a previous consumer check because publisher
        // and merger delay do not change their timestamps. Consumer time cannot.
        let latest_observed_time_ns = self
            .last_consumer_time_ns
            .max(self.last_candidate_receipt_time_ns)
            .max(self.last_left_receipt_time_ns)
            .max(self.last_right_receipt_time_ns);
        if latest_observed_time_ns.is_some_and(|previous| host_time_ns < previous) {
            self.terminate_neutral_preserving_primary(
                LiveTerminalReason::HostClockRegression,
                FaultReason::DecoderReset,
            );
            return Err(LiveCalibratedError::HostClockRegression);
        }
        self.last_consumer_time_ns = Some(host_time_ns);
        self.enforce_watchdog(host_time_ns)?;
        // Stale warmup has no output lease to revoke. Keep processing its
        // clock evidence, but withhold arming until both raw streams are fresh.
        Ok(self.runtime.is_armed()
            || (Self::receipt_is_live(self.last_left_receipt_time_ns, host_time_ns)
                && Self::receipt_is_live(self.last_right_receipt_time_ns, host_time_ns)))
    }

    /// Permanently revoke this live instance when one source disappears.
    pub fn source_lost(&mut self, hand: Hand) -> Result<(), LiveCalibratedError<A::Error>> {
        self.ensure_active()?;
        self.terminate_neutral(
            LiveTerminalReason::SourceLost(hand),
            FaultReason::TransportDisconnect,
        )
    }

    /// Permanently revoke this live instance when the input channel disappears.
    pub fn disconnect(&mut self) -> Result<(), LiveCalibratedError<A::Error>> {
        self.ensure_active()?;
        self.terminate_neutral(
            LiveTerminalReason::Disconnected,
            FaultReason::TransportDisconnect,
        )
    }

    /// Permanently stop output for a planned SIGINT or SIGTERM.
    pub fn shutdown(
        &mut self,
        signal: ShutdownSignal,
    ) -> Result<(), LiveCalibratedError<A::Error>> {
        self.ensure_active()?;
        self.mark_terminal(LiveTerminalReason::PlannedShutdown(signal));
        let shutdown = self.runtime.shutdown(signal);
        self.sync_output_neutrality();
        shutdown.map_err(live_runtime_error)
    }

    /// Complete a deliberate local shutdown without processing merge backlog.
    /// A clean finish is not a terminal fault, but consuming `self` prevents
    /// this user attestation from authorizing another live instance.
    pub fn finish(mut self) -> Result<LiveCalibratedExecution<A>, LiveCalibratedError<A::Error>> {
        self.pending.clear();
        if self.report.terminal_reason.is_none() {
            let revocation = self.runtime.revoke_run_ownership();
            self.sync_output_neutrality();
            revocation.map_err(live_runtime_error)?;
            self.report.finished_cleanly = true;
        }
        self.report.final_key_state_neutral = self.runtime.pressed().is_empty();
        self.sync_output_neutrality();
        let neutralization_failure = self.runtime.take_neutralization_failure();
        let left_clock_fit = self.left_clock.current_fit().copied();
        let right_clock_fit = self.right_clock.current_fit().copied();
        let left_clock_diagnostics = self.left_clock.diagnostics();
        let right_clock_diagnostics = self.right_clock.diagnostics();
        Ok(LiveCalibratedExecution {
            report: self.report,
            output: self.runtime.into_output(),
            neutralization_failure,
            left_clock_fit,
            right_clock_fit,
            left_clock_diagnostics,
            right_clock_diagnostics,
        })
    }

    #[must_use]
    pub const fn report(&self) -> &LiveCalibratedReport {
        &self.report
    }

    fn observe_clock(
        &mut self,
        frame: &SensorFrame,
        receipt_time_ns: u64,
    ) -> Result<Option<u64>, LiveCalibratedError<A::Error>> {
        let result = match frame.hand {
            Hand::Left => {
                self.left_clock
                    .observe(frame.boot_id, frame.device_time_us, receipt_time_ns)
            }
            Hand::Right => {
                self.right_clock
                    .observe(frame.boot_id, frame.device_time_us, receipt_time_ns)
            }
        };
        match result {
            Ok(mapped) => Ok(mapped),
            Err(error) => {
                self.terminate_neutral_preserving_primary(
                    LiveTerminalReason::ClockFailure(frame.hand),
                    FaultReason::DecoderReset,
                );
                Err(LiveCalibratedError::Clock {
                    hand: frame.hand,
                    error,
                })
            }
        }
    }

    fn clocks_ready(&self) -> bool {
        self.left_clock.current_fit().is_some() && self.right_clock.current_fit().is_some()
    }

    fn observe_hand_receipt(&mut self, hand: Hand, receipt_time_ns: u64) {
        match hand {
            Hand::Left => {
                self.last_left_receipt_time_ns =
                    self.last_left_receipt_time_ns.max(Some(receipt_time_ns));
            }
            Hand::Right => {
                self.last_right_receipt_time_ns =
                    self.last_right_receipt_time_ns.max(Some(receipt_time_ns));
            }
        }
    }

    fn receipt_is_live(receipt_time_ns: Option<u64>, host_time_ns: u64) -> bool {
        receipt_time_ns.is_some_and(|last| host_time_ns.saturating_sub(last) <= LIVE_WATCHDOG_NS)
    }

    fn enforce_watchdog(&mut self, host_time_ns: u64) -> Result<(), LiveCalibratedError<A::Error>> {
        if !self.runtime.is_armed() {
            return Ok(());
        }
        let left_expired = !Self::receipt_is_live(self.last_left_receipt_time_ns, host_time_ns);
        let right_expired = !Self::receipt_is_live(self.last_right_receipt_time_ns, host_time_ns);
        if !left_expired && !right_expired {
            return Ok(());
        }

        self.report.watchdog_timeouts += 1;
        self.report.left_watchdog_timeouts += u64::from(left_expired);
        self.report.right_watchdog_timeouts += u64::from(right_expired);
        let reason = LiveTerminalReason::WatchdogTimeout {
            left_expired,
            right_expired,
        };
        self.terminate_neutral_preserving_primary(reason.clone(), FaultReason::WatchdogTimeout);
        Err(LiveCalibratedError::TerminalFault(reason))
    }

    fn flush(&mut self, watermark: Option<u64>) -> Result<(), LiveCalibratedError<A::Error>> {
        loop {
            let next = self
                .pending
                .iter()
                .enumerate()
                .filter(|(_, frame)| watermark.is_none_or(|limit| frame.mapped_time_ns <= limit))
                .min_by_key(|(_, frame)| {
                    (frame.mapped_time_ns, frame.receipt_time_ns, frame.ordinal)
                })
                .map(|(index, _)| index);
            let Some(index) = next else {
                return Ok(());
            };
            let pending = self.pending.remove(index);
            if self
                .highest_processed_ordinal
                .is_some_and(|last| pending.ordinal < last)
            {
                self.report.reordered_frames += 1;
            }
            self.highest_processed_ordinal = Some(
                self.highest_processed_ordinal
                    .map_or(pending.ordinal, |last| last.max(pending.ordinal)),
            );
            let result = self
                .runtime
                .push_frame(pending.frame, pending.mapped_time_ns / 1_000);
            self.observe_runtime(result)?;
        }
    }

    fn observe_runtime(
        &mut self,
        result: Option<Result<RuntimeStep, RuntimeError<A::Error>>>,
    ) -> Result<(), LiveCalibratedError<A::Error>> {
        let Some(result) = result else {
            return Ok(());
        };
        self.sync_output_neutrality();
        match result {
            Ok(step) => {
                self.report.decisions += 1;
                match step.decision.kind {
                    DecisionKind::Key(_) => self.report.key_decisions += 1,
                    DecisionKind::Abstain(_) => self.report.abstentions += 1,
                }
                self.report.output_events += u64::try_from(step.events.len()).unwrap_or(u64::MAX);
                if let DecisionKind::Abstain(reason) = step.decision.kind
                    && reason.invalidates_live_output()
                {
                    let terminal = LiveTerminalReason::EvidenceInvalidatingAbstention(reason);
                    // The inner runtime already revoked its lease for this
                    // decision. Clear only wrapper-owned evidence here.
                    self.mark_terminal(terminal.clone());
                    return Err(LiveCalibratedError::TerminalFault(terminal));
                }
                Ok(())
            }
            Err(RuntimeError::Output(error)) => {
                self.mark_terminal(LiveTerminalReason::OutputFailure);
                Err(LiveCalibratedError::Output(error))
            }
            Err(RuntimeError::Gate(error)) => {
                self.terminate_neutral_preserving_primary(
                    LiveTerminalReason::OutputGateFailure,
                    FaultReason::DecoderReset,
                );
                Err(LiveCalibratedError::Gate(error))
            }
            Err(error) => {
                let fault = runtime_fault(error);
                self.report.runtime_faults.push(fault.clone());
                let terminal = LiveTerminalReason::RuntimeFault(fault);
                // Every non-output runtime error is returned only after the
                // inner safety owner attempted neutral recovery.
                self.mark_terminal(terminal.clone());
                Err(LiveCalibratedError::TerminalFault(terminal))
            }
        }
    }

    fn force_runtime_neutral(
        &mut self,
        reason: FaultReason,
    ) -> Result<(), LiveCalibratedError<A::Error>> {
        let result = self.runtime.force_neutral(reason);
        self.sync_output_neutrality();
        match result {
            Ok(()) => Ok(()),
            Err(RuntimeError::Output(error)) => Err(LiveCalibratedError::Output(error)),
            Err(error) => {
                self.report.runtime_faults.push(runtime_fault(error));
                Ok(())
            }
        }
    }

    fn terminate_neutral(
        &mut self,
        terminal_reason: LiveTerminalReason,
        reason: FaultReason,
    ) -> Result<(), LiveCalibratedError<A::Error>> {
        self.mark_terminal(terminal_reason);
        self.force_runtime_neutral(reason)
    }

    fn terminate_neutral_preserving_primary(
        &mut self,
        terminal_reason: LiveTerminalReason,
        reason: FaultReason,
    ) {
        self.mark_terminal(terminal_reason);
        self.runtime.force_neutral_preserving_primary(reason);
        self.sync_output_neutrality();
    }

    fn mark_terminal(&mut self, reason: LiveTerminalReason) {
        if self.report.terminal_reason.is_none() {
            self.report.terminal_reason = Some(reason);
        }
        self.pending.clear();
        self.reset_clock_evidence();
    }

    fn ensure_active(&self) -> Result<(), LiveCalibratedError<A::Error>> {
        match &self.report.terminal_reason {
            Some(reason) => Err(LiveCalibratedError::WearSessionRevoked(reason.clone())),
            None => Ok(()),
        }
    }

    fn fail_output_gate(
        &mut self,
        error: OutputGateError,
    ) -> Result<(), LiveCalibratedError<A::Error>> {
        self.terminate_neutral_preserving_primary(
            LiveTerminalReason::OutputGateFailure,
            FaultReason::DecoderReset,
        );
        Err(LiveCalibratedError::Gate(error))
    }

    fn sync_output_neutrality(&mut self) {
        self.report.output_neutrality_confirmed = self.runtime.output_neutrality_confirmed();
    }

    fn reset_clock_evidence(&mut self) {
        self.left_clock.reset(self.left_identity.boot_id);
        self.right_clock.reset(self.right_identity.boot_id);
        self.last_left_receipt_time_ns = None;
        self.last_right_receipt_time_ns = None;
    }
}

fn invalid_candidate_fault(
    candidate: &SourcedTransportCandidate,
) -> (CalibratedRuntimeFault, FaultReason) {
    match &candidate.candidate {
        TransportCandidate::Decoded(_) => {
            let error = candidate
                .frame
                .as_ref()
                .expect("decoded candidate has a cached frame result")
                .as_ref()
                .expect_err("unavailable frame identity has a cached decode error");
            (
                CalibratedRuntimeFault::Frame(*error),
                FaultReason::MalformedFrame,
            )
        }
        TransportCandidate::Malformed { .. } => (
            CalibratedRuntimeFault::Transport(TransportFault::Malformed),
            FaultReason::MalformedFrame,
        ),
        TransportCandidate::Oversized { actual_len, .. } => (
            CalibratedRuntimeFault::Transport(TransportFault::Oversized {
                actual_len: *actual_len,
            }),
            FaultReason::MalformedFrame,
        ),
        TransportCandidate::Unterminated { .. } => (
            CalibratedRuntimeFault::Transport(TransportFault::Unterminated),
            FaultReason::TransportDisconnect,
        ),
    }
}

fn runtime_fault<E>(error: RuntimeError<E>) -> CalibratedRuntimeFault {
    match error {
        RuntimeError::Transport(error) => CalibratedRuntimeFault::Transport(error),
        RuntimeError::Frame(error) => CalibratedRuntimeFault::Frame(error),
        RuntimeError::Pipeline(error) => CalibratedRuntimeFault::Pipeline(error),
        RuntimeError::UnconfiguredHand(hand) => CalibratedRuntimeFault::UnconfiguredHand(hand),
        RuntimeError::State(error) => CalibratedRuntimeFault::State(error),
        RuntimeError::Gate(_) => unreachable!("run gate errors are handled before fault mapping"),
        RuntimeError::Output(_) => unreachable!("output errors are handled before fault mapping"),
    }
}

fn live_start_error<E>(error: RuntimeStartError<E>) -> LiveCalibratedError<E> {
    match error {
        RuntimeStartError::Configuration(error) => LiveCalibratedError::RuntimeConfiguration(error),
        RuntimeStartError::Output(cause) => LiveCalibratedError::Output(RuntimeOutputError {
            event: None,
            cause,
            recovery_error: None,
        }),
    }
}

fn live_runtime_error<E>(error: RuntimeError<E>) -> LiveCalibratedError<E> {
    match error {
        RuntimeError::Output(error) => LiveCalibratedError::Output(error),
        RuntimeError::Gate(error) => LiveCalibratedError::Gate(error),
        _ => unreachable!("lifecycle operation produced a decoder fault"),
    }
}

#[derive(Debug)]
pub enum LiveCalibratedError<E> {
    Binding(RuntimeBindingError),
    RuntimeConfiguration(RuntimeConfigurationError),
    ForeignAdmission,
    Gate(OutputGateError),
    TerminalFault(LiveTerminalReason),
    WearSessionRevoked(LiveTerminalReason),
    HostReceiptRegression { ordinal: u64 },
    HostClockRegression,
    MappedReceiptDeltaTooLarge { ordinal: u64, actual_ns: u64 },
    FrameCanonicalization(FrameCanonicalizationError),
    Clock { hand: Hand, error: ClockError },
    Output(RuntimeOutputError<E>),
}

impl<E: fmt::Display> fmt::Display for LiveCalibratedError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Binding(error) => error.fmt(formatter),
            Self::RuntimeConfiguration(error) => {
                write!(formatter, "live decoder topology is invalid: {error}")
            }
            Self::ForeignAdmission => {
                formatter.write_str("live receipt-merger update belongs to another runtime")
            }
            Self::Gate(error) => write!(formatter, "live output run gate failed: {error:?}"),
            Self::TerminalFault(reason) => {
                write!(formatter, "live wear session was revoked: {reason:?}")
            }
            Self::WearSessionRevoked(reason) => write!(
                formatter,
                "live wear session is already revoked: {reason:?}"
            ),
            Self::HostReceiptRegression { ordinal } => {
                write!(
                    formatter,
                    "live host receipt time regresses at candidate {ordinal}"
                )
            }
            Self::HostClockRegression => formatter.write_str("live host tick time regresses"),
            Self::MappedReceiptDeltaTooLarge { ordinal, actual_ns } => write!(
                formatter,
                "live candidate {ordinal} mapped/receipt delta is {actual_ns} ns; maximum is {MAX_MAPPED_RECEIPT_DELTA_NS} ns"
            ),
            Self::FrameCanonicalization(error) => write!(
                formatter,
                "live frame cannot enter canonical sensor coordinates: {error}"
            ),
            Self::Clock { hand, error } => {
                write!(formatter, "live {hand:?} clock failed: {error}")
            }
            Self::Output(error) => {
                write!(formatter, "live output failed: {}", error.cause)?;
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

impl<E: Error + 'static> Error for LiveCalibratedError<E> {}
