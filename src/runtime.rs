//! One canonical live path from transport bytes to fail-neutral key events.

use std::collections::BTreeSet;

use crate::{
    decoder::{Decision, DecisionKind, HandDecoder},
    layout::{Hand, KeyUsage},
    output::{
        ApplyAnomaly, FaultReason, KeyEvent, OutputAdapter, OutputGateError, OutputRunLease,
        OutputSafetyError, OutputSafetySession, RequiredSourceTopology, ShutdownSignal,
    },
    pipeline::{CausalPipeline, PipelineError},
    protocol::{FrameError, SensorFrame, decode_frame},
    transport::{TransportCandidate, TransportDecoder},
};

/// A transport candidate that cannot enter the frame decoder.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransportFault {
    Malformed,
    Oversized { actual_len: usize },
    Unterminated,
}

/// One live-path failure after output recovery was attempted.
#[derive(Debug, PartialEq)]
pub enum RuntimeError<E> {
    Transport(TransportFault),
    Frame(FrameError),
    Pipeline(PipelineError),
    UnconfiguredHand(Hand),
    State(ApplyAnomaly),
    Gate(OutputGateError),
    Output(RuntimeOutputError<E>),
}

/// One adapter failure with the result of its independent all-up attempt.
#[derive(Debug, PartialEq)]
pub struct RuntimeOutputError<E> {
    pub event: Option<KeyEvent>,
    pub cause: E,
    pub recovery_error: Option<E>,
}

impl<E> RuntimeOutputError<E> {
    /// Return whether the independent all-up path completed successfully.
    #[must_use]
    pub const fn output_neutrality_confirmed(&self) -> bool {
        self.event.is_some() && self.recovery_error.is_none()
    }
}

/// Constructor failure before a runtime can accept evidence.
#[derive(Debug, PartialEq)]
pub enum RuntimeStartError<E> {
    Configuration(RuntimeConfigurationError),
    Output(E),
}

/// Invalid decoder topology rejected before output state exists.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeConfigurationError {
    ExpectedHand { expected: Hand, actual: Hand },
}

/// One emitted decoder result and its exact output transitions.
#[derive(Clone, Debug, PartialEq)]
pub struct RuntimeStep {
    pub decision: Decision,
    pub events: Vec<KeyEvent>,
}

/// COBS transport and per-hand causal decoding around one safety owner.
///
/// [`OutputSafetySession`] is the only owner of key state and the concrete
/// adapter. This layer owns decoder context, transport framing, and the current
/// run lease only.
pub struct LiveRuntime<A: OutputAdapter> {
    transport: TransportDecoder,
    left: Option<CausalPipeline>,
    right: Option<CausalPipeline>,
    safety: OutputSafetySession<A>,
    run_lease: Option<OutputRunLease>,
    last_time_us: u64,
    output_neutrality_confirmed: bool,
    neutralization_failure: Option<RuntimeOutputError<A::Error>>,
}

impl<A: OutputAdapter> LiveRuntime<A> {
    /// Construct a disarmed one-hand falsification rig from neutral output.
    ///
    /// # Errors
    ///
    /// Returns the adapter error when startup neutralization fails.
    pub fn one_hand(decoder: HandDecoder, output: A) -> Result<Self, RuntimeStartError<A::Error>> {
        let hand = decoder.hand();
        let (left, right) = match hand {
            Hand::Left => (Some(CausalPipeline::new(decoder)), None),
            Hand::Right => (None, Some(CausalPipeline::new(decoder))),
        };
        let safety = OutputSafetySession::start(output, RequiredSourceTopology::One(hand))
            .map_err(RuntimeStartError::Output)?;
        Ok(Self {
            transport: TransportDecoder::new(),
            left,
            right,
            safety,
            run_lease: None,
            last_time_us: 0,
            output_neutrality_confirmed: true,
            neutralization_failure: None,
        })
    }

    /// Construct the canonical disarmed two-hand runtime in fixed order.
    ///
    /// # Errors
    ///
    /// Rejects swapped or duplicated decoder identities, then fails when
    /// startup output neutralization cannot be established.
    pub fn two_hand(
        left_decoder: HandDecoder,
        right_decoder: HandDecoder,
        output: A,
    ) -> Result<Self, RuntimeStartError<A::Error>> {
        if left_decoder.hand() != Hand::Left {
            return Err(RuntimeStartError::Configuration(
                RuntimeConfigurationError::ExpectedHand {
                    expected: Hand::Left,
                    actual: left_decoder.hand(),
                },
            ));
        }
        if right_decoder.hand() != Hand::Right {
            return Err(RuntimeStartError::Configuration(
                RuntimeConfigurationError::ExpectedHand {
                    expected: Hand::Right,
                    actual: right_decoder.hand(),
                },
            ));
        }
        let safety = OutputSafetySession::start(output, RequiredSourceTopology::TwoHands)
            .map_err(RuntimeStartError::Output)?;
        Ok(Self {
            transport: TransportDecoder::new(),
            left: Some(CausalPipeline::new(left_decoder)),
            right: Some(CausalPipeline::new(right_decoder)),
            safety,
            run_lease: None,
            last_time_us: 0,
            output_neutrality_confirmed: true,
            neutralization_failure: None,
        })
    }

    /// Mark one required source live without arming output.
    pub fn source_connected(&mut self, hand: Hand) -> Result<(), OutputGateError> {
        self.safety.source_connected(hand)
    }

    /// Acquire the sole explicit output-run lease after required sources live.
    pub fn acquire_run_ownership(&mut self) -> Result<(), OutputGateError> {
        if self.run_lease.is_some() {
            return Err(OutputGateError::RunAlreadyOwned);
        }
        self.run_lease = Some(self.safety.acquire_run_ownership()?);
        Ok(())
    }

    /// Consume any byte chunk and return every decision or fault it completes.
    pub fn push_bytes(&mut self, bytes: &[u8]) -> Vec<Result<RuntimeStep, RuntimeError<A::Error>>> {
        let candidates = self.transport.push(bytes);
        let mut results = Vec::new();
        for candidate in candidates {
            if let Some(result) = self.process_candidate(candidate) {
                results.push(result);
            }
        }
        results
    }

    /// Consume one candidate already preserved by the session boundary.
    pub fn push_candidate(
        &mut self,
        candidate: TransportCandidate,
    ) -> Option<Result<RuntimeStep, RuntimeError<A::Error>>> {
        self.process_candidate(candidate)
    }

    /// Consume one raw-timed frame with its explicit decision/output time.
    pub fn push_frame(
        &mut self,
        raw_frame: SensorFrame,
        output_time_us: u64,
    ) -> Option<Result<RuntimeStep, RuntimeError<A::Error>>> {
        self.process_frame(raw_frame, output_time_us)
    }

    /// End transport evidence and revoke every required source.
    pub fn disconnect(&mut self) -> Vec<Result<RuntimeStep, RuntimeError<A::Error>>> {
        let trailing = self
            .transport
            .finish()
            .and_then(|candidate| self.process_candidate(candidate));
        self.reset_pipelines();
        let mut results = trailing.into_iter().collect::<Vec<_>>();
        let configured_hands = [
            self.left.as_ref().map(|_| Hand::Left),
            self.right.as_ref().map(|_| Hand::Right),
        ];
        for hand in configured_hands.into_iter().flatten() {
            if let Err(error) = self.source_lost(hand) {
                results.push(Err(error));
                break;
            }
        }
        results
    }

    /// Revoke one source and force neutral through the canonical owner.
    pub fn source_lost(&mut self, hand: Hand) -> Result<(), RuntimeError<A::Error>> {
        match self.safety.source_lost(hand, self.last_time_us) {
            Err(OutputSafetyError::Gate(error)) => Err(RuntimeError::Gate(error)),
            recovery => {
                self.reset_pipelines();
                self.run_lease = None;
                self.observe_safety_result(recovery).map(|_| ())
            }
        }
    }

    /// Reset decoder context and revoke run ownership for a watchdog fault.
    pub fn watchdog_timeout(&mut self) -> Result<(), RuntimeError<A::Error>> {
        self.force_neutral(FaultReason::WatchdogTimeout)
    }

    /// Reset decoder context and revoke output for one external runtime fault.
    pub(crate) fn force_neutral(
        &mut self,
        reason: FaultReason,
    ) -> Result<(), RuntimeError<A::Error>> {
        self.reset_pipelines();
        self.revoke_run(reason)
    }

    /// Force neutral while retaining an adapter failure behind another cause.
    pub(crate) fn force_neutral_preserving_primary(&mut self, reason: FaultReason) {
        if let Err(RuntimeError::Output(failure)) = self.force_neutral(reason) {
            self.retain_neutralization_failure(failure);
        }
    }

    /// Revoke explicit run ownership without claiming desktop focus control.
    pub fn revoke_run_ownership(&mut self) -> Result<(), RuntimeError<A::Error>> {
        let Some(lease) = self.run_lease.take() else {
            return Ok(());
        };
        self.reset_pipelines();
        let recovery = self.safety.revoke_run_ownership(lease, self.last_time_us);
        self.observe_safety_result(recovery).map(|_| ())
    }

    /// Permanently stop output under planned SIGINT or SIGTERM semantics.
    pub fn shutdown(&mut self, signal: ShutdownSignal) -> Result<(), RuntimeError<A::Error>> {
        self.reset_pipelines();
        self.run_lease = None;
        let recovery = self.safety.shutdown(signal, self.last_time_us);
        self.observe_safety_result(recovery).map(|_| ())
    }

    /// Return the concrete adapter for evidence inspection.
    #[must_use]
    pub const fn output(&self) -> &A {
        self.safety.output()
    }

    /// Consume the already-neutral runtime and return its adapter.
    #[must_use]
    pub fn into_output(self) -> A {
        self.safety.into_output()
    }

    /// Return the canonical software key-down set.
    #[must_use]
    pub const fn pressed(&self) -> &BTreeSet<KeyUsage> {
        self.safety.pressed()
    }

    /// Return whether the most recent all-up path completed at the adapter.
    #[must_use]
    pub const fn output_neutrality_confirmed(&self) -> bool {
        self.output_neutrality_confirmed
    }

    /// Drain an adapter failure retained behind a primary runtime cause.
    pub fn take_neutralization_failure(&mut self) -> Option<RuntimeOutputError<A::Error>> {
        self.neutralization_failure.take()
    }

    /// Return whether this runtime currently owns an armed output run.
    #[must_use]
    pub fn is_armed(&self) -> bool {
        self.run_lease.is_some() && self.safety.is_run_owned()
    }

    /// Return buffered causal context for one configured hand.
    #[must_use]
    pub fn buffered_frames(&self, hand: Hand) -> Option<usize> {
        match hand {
            Hand::Left => self.left.as_ref(),
            Hand::Right => self.right.as_ref(),
        }
        .map(CausalPipeline::buffered_frames)
    }

    fn process_candidate(
        &mut self,
        candidate: TransportCandidate,
    ) -> Option<Result<RuntimeStep, RuntimeError<A::Error>>> {
        match candidate {
            TransportCandidate::Decoded(payload) => match decode_frame(&payload) {
                Ok(frame) => self.process_frame(frame, frame.device_time_us),
                Err(error) => {
                    Some(self.fail(FaultReason::MalformedFrame, RuntimeError::Frame(error)))
                }
            },
            TransportCandidate::Malformed { .. } => Some(self.fail(
                FaultReason::MalformedFrame,
                RuntimeError::Transport(TransportFault::Malformed),
            )),
            TransportCandidate::Oversized { actual_len, .. } => Some(self.fail(
                FaultReason::MalformedFrame,
                RuntimeError::Transport(TransportFault::Oversized { actual_len }),
            )),
            TransportCandidate::Unterminated { .. } => Some(self.fail(
                FaultReason::TransportDisconnect,
                RuntimeError::Transport(TransportFault::Unterminated),
            )),
        }
    }

    fn process_frame(
        &mut self,
        raw_frame: SensorFrame,
        output_time_us: u64,
    ) -> Option<Result<RuntimeStep, RuntimeError<A::Error>>> {
        self.last_time_us = self.last_time_us.max(output_time_us);
        let pipeline = match raw_frame.hand {
            Hand::Left => self.left.as_mut(),
            Hand::Right => self.right.as_mut(),
        };
        let Some(pipeline) = pipeline else {
            return Some(self.fail(
                FaultReason::DecoderReset,
                RuntimeError::UnconfiguredHand(raw_frame.hand),
            ));
        };
        let mut decision = match pipeline.push(raw_frame) {
            Ok(Some(decision)) => decision,
            Ok(None) => return None,
            Err(error) => {
                return Some(self.fail(FaultReason::DecoderReset, RuntimeError::Pipeline(error)));
            }
        };
        decision.decision_time_us = output_time_us;

        if is_evidence_fault(decision) {
            // The abstention is the operational cause. The safety owner still
            // revokes and permanently stops itself if neutralization fails,
            // but that secondary adapter error must not replace the decision.
            self.force_neutral_preserving_primary(FaultReason::DecoderReset);
            return Some(Ok(RuntimeStep {
                decision,
                events: Vec::new(),
            }));
        }

        let DecisionKind::Key(usage) = decision.kind else {
            return Some(Ok(RuntimeStep {
                decision,
                events: Vec::new(),
            }));
        };
        let Some(lease) = self.run_lease else {
            return Some(Ok(RuntimeStep {
                decision,
                events: Vec::new(),
            }));
        };
        let applied = self.safety.tap(lease, usage, decision.decision_time_us);
        match self.observe_safety_result(applied) {
            Ok(applied) => Some(Ok(RuntimeStep {
                decision,
                events: applied.events,
            })),
            Err(error) => {
                self.run_lease = None;
                self.reset_pipelines();
                Some(Err(error))
            }
        }
    }

    fn fail(
        &mut self,
        reason: FaultReason,
        error: RuntimeError<A::Error>,
    ) -> Result<RuntimeStep, RuntimeError<A::Error>> {
        // Preserve the first decoder/transport failure. `force_neutral` clears
        // authority before touching the adapter, and the safety owner stops
        // itself if recovery fails. Retain that secondary error separately
        // while returning the first decoder or transport cause.
        self.force_neutral_preserving_primary(reason);
        Err(error)
    }

    fn revoke_run(&mut self, reason: FaultReason) -> Result<(), RuntimeError<A::Error>> {
        self.run_lease = None;
        let recovery = self
            .safety
            .force_neutral_for_runtime(reason, self.last_time_us);
        self.observe_safety_result(recovery).map(|_| ())
    }

    fn observe_safety_result<T>(
        &mut self,
        result: Result<T, OutputSafetyError<A::Error>>,
    ) -> Result<T, RuntimeError<A::Error>> {
        match result {
            Ok(value) => {
                self.output_neutrality_confirmed = true;
                Ok(value)
            }
            Err(error) => Err(self.runtime_safety_error(error)),
        }
    }

    fn runtime_safety_error(
        &mut self,
        error: OutputSafetyError<A::Error>,
    ) -> RuntimeError<A::Error> {
        match error {
            OutputSafetyError::Gate(error) => RuntimeError::Gate(error),
            OutputSafetyError::State(error) => {
                self.output_neutrality_confirmed = true;
                RuntimeError::State(error)
            }
            OutputSafetyError::Adapter {
                event,
                cause,
                recovery_error,
            } => {
                let failure = RuntimeOutputError {
                    event,
                    cause,
                    recovery_error,
                };
                self.output_neutrality_confirmed = failure.output_neutrality_confirmed();
                RuntimeError::Output(failure)
            }
        }
    }

    fn retain_neutralization_failure(&mut self, failure: RuntimeOutputError<A::Error>) {
        if self.neutralization_failure.is_none() {
            self.neutralization_failure = Some(failure);
        }
    }

    fn reset_pipelines(&mut self) {
        if let Some(pipeline) = &mut self.left {
            pipeline.reset();
        }
        if let Some(pipeline) = &mut self.right {
            pipeline.reset();
        }
    }
}

fn is_evidence_fault(decision: Decision) -> bool {
    matches!(
        decision.kind,
        DecisionKind::Abstain(reason) if reason.invalidates_live_output()
    )
}
