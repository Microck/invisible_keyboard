use invisible_keyboard::{
    layout::{CORE28_KEY_COUNT, Hand, KeyUsage, all_slots},
    output::{
        ApplyAnomaly, EventState, FaultReason, KeyEvent, KeyStateMachine, OUTPUT_SYSTEM_TEST_GATES,
        OutputAdapter, OutputGateError, OutputRunLease, OutputSafetyError, OutputSafetySession,
        OutputSystemTestGate, RecordingOutput, RequiredSourceTopology, ShutdownSignal, emit_events,
    },
};

#[test]
fn core28_tap_is_one_down_up_pair() {
    let mut state = KeyStateMachine::new();
    let result = state.tap(KeyUsage::A, 10_000);

    assert_eq!(result.anomaly, None);
    assert_eq!(
        result.events,
        vec![
            KeyEvent::down(KeyUsage::A, 10_000),
            KeyEvent::up(KeyUsage::A, 10_000),
        ]
    );
    assert!(state.pressed().is_empty());
}

#[test]
fn duplicate_down_and_orphan_up_never_reach_output() {
    let mut state = KeyStateMachine::new();
    assert_eq!(
        state.apply(KeyEvent::down(KeyUsage::Space, 1)).anomaly,
        None
    );

    let duplicate = state.apply(KeyEvent::down(KeyUsage::Space, 2));
    assert_eq!(duplicate.events, vec![]);
    assert_eq!(duplicate.anomaly, Some(ApplyAnomaly::DuplicateDown));

    assert_eq!(state.apply(KeyEvent::up(KeyUsage::Space, 3)).anomaly, None);
    let orphan = state.apply(KeyEvent::up(KeyUsage::Space, 4));
    assert_eq!(orphan.events, vec![]);
    assert_eq!(orphan.anomaly, Some(ApplyAnomaly::OrphanUp));
    assert!(state.pressed().is_empty());
}

#[test]
fn every_fault_releases_all_keys_in_stable_order() {
    for reason in [
        FaultReason::DecoderReset,
        FaultReason::TransportDisconnect,
        FaultReason::MalformedFrame,
        FaultReason::WatchdogTimeout,
        FaultReason::OutputFailure,
        FaultReason::RunOwnershipRevoked,
        FaultReason::ProcessInterrupt,
        FaultReason::ProcessTerminate,
    ] {
        let mut state = KeyStateMachine::new();
        state.apply(KeyEvent::down(KeyUsage::Z, 10));
        state.apply(KeyEvent::down(KeyUsage::A, 11));
        state.apply(KeyEvent::down(KeyUsage::Space, 12));

        let release = state.fault(reason, 20);
        assert_eq!(release.reason, reason);
        assert_eq!(
            release.events,
            vec![
                KeyEvent::up(KeyUsage::A, 20),
                KeyEvent::up(KeyUsage::Z, 20),
                KeyEvent::up(KeyUsage::Space, 20),
            ]
        );
        assert!(state.pressed().is_empty());
    }
}

#[test]
fn time_regression_fails_neutral_and_does_not_apply_requested_event() {
    let mut state = KeyStateMachine::new();
    state.apply(KeyEvent::down(KeyUsage::B, 100));
    state.apply(KeyEvent::down(KeyUsage::A, 110));

    let result = state.apply(KeyEvent::down(KeyUsage::C, 90));
    assert_eq!(
        result.anomaly,
        Some(ApplyAnomaly::TimeRegression {
            previous_us: 110,
            actual_us: 90,
        })
    );
    assert_eq!(
        result.events,
        vec![
            KeyEvent::up(KeyUsage::A, 110),
            KeyEvent::up(KeyUsage::B, 110),
        ]
    );
    assert!(!result.events.iter().any(|event| event.usage == KeyUsage::C));
    assert!(state.pressed().is_empty());
}

#[test]
fn deterministic_replay_crosses_the_output_adapter_seam() {
    let events = [
        KeyEvent::down(KeyUsage::H, 1),
        KeyEvent::up(KeyUsage::H, 2),
        KeyEvent::down(KeyUsage::I, 3),
        KeyEvent::up(KeyUsage::I, 4),
    ];
    let mut output = RecordingOutput::new();

    let receipts = emit_events(&mut output, &events).expect("recording adapter cannot fail");

    assert_eq!(receipts.len(), events.len());
    assert_eq!(output.events(), events);
    assert_eq!(output.release_all_calls(), 0);

    output.release_all().expect("recording adapter cannot fail");
    assert_eq!(output.release_all_calls(), 1);
}

fn armed_recording_session() -> (OutputSafetySession<RecordingOutput>, OutputRunLease) {
    let mut session =
        OutputSafetySession::start(RecordingOutput::new(), RequiredSourceTopology::TwoHands)
            .expect("recording output always reaches neutral startup");
    session
        .source_connected(Hand::Left)
        .expect("left belongs to two-hand topology");
    session
        .source_connected(Hand::Right)
        .expect("right belongs to two-hand topology");
    let lease = session
        .acquire_run_ownership()
        .expect("both live sources permit one run owner");
    (session, lease)
}

#[test]
fn output_requires_declared_sources_and_one_run_owner() {
    let mut session =
        OutputSafetySession::start(RecordingOutput::new(), RequiredSourceTopology::TwoHands)
            .expect("recording output always reaches neutral startup");
    assert_eq!(
        session.acquire_run_ownership(),
        Err(OutputGateError::SourcesUnavailable)
    );
    session
        .source_connected(Hand::Left)
        .expect("left belongs to two-hand topology");
    assert_eq!(
        session.acquire_run_ownership(),
        Err(OutputGateError::SourcesUnavailable)
    );
    session
        .source_connected(Hand::Right)
        .expect("right belongs to two-hand topology");
    session
        .acquire_run_ownership()
        .expect("two live sources may arm one run");
    assert_eq!(
        session.acquire_run_ownership(),
        Err(OutputGateError::RunAlreadyOwned)
    );

    for required in [Hand::Left, Hand::Right] {
        let mut one = OutputSafetySession::start(
            RecordingOutput::new(),
            RequiredSourceTopology::One(required),
        )
        .expect("one-hand session reaches neutral startup");
        let other = match required {
            Hand::Left => Hand::Right,
            Hand::Right => Hand::Left,
        };
        assert_eq!(
            one.source_connected(other),
            Err(OutputGateError::UnexpectedSource)
        );
        one.source_connected(required)
            .expect("declared one-hand source connects");
        one.acquire_run_ownership()
            .expect("one declared live source arms the run");
    }
}

#[test]
fn every_core28_usage_crosses_the_armed_output_session_as_one_pair() {
    let (mut session, lease) = armed_recording_session();
    let usages = all_slots()
        .iter()
        .filter_map(|slot| slot.usage)
        .collect::<Vec<_>>();

    for (ordinal, usage) in usages.iter().copied().enumerate() {
        let time_us = u64::try_from(ordinal).expect("Core-28 ordinal fits u64");
        let applied = session
            .tap(lease, usage, time_us)
            .expect("an armed Core-28 tap reaches output");
        assert_eq!(
            applied.events,
            [KeyEvent::down(usage, time_us), KeyEvent::up(usage, time_us)]
        );
    }

    assert_eq!(usages.len(), CORE28_KEY_COUNT);
    assert_eq!(session.output().events().len(), 56);
    assert!(session.pressed().is_empty());
    assert_eq!(session.output().release_all_calls(), 1);
}

#[test]
fn source_loss_during_a_transition_releases_and_requires_a_fresh_lease() {
    for lost_source in [Hand::Left, Hand::Right] {
        let (mut session, lease) = armed_recording_session();
        session
            .apply(lease, KeyEvent::down(KeyUsage::A, 1))
            .expect("the active lease accepts a down transition");

        let release = session
            .source_lost(lost_source, 2)
            .expect("source loss invokes independent all-up recovery");

        assert_eq!(release.reason, FaultReason::TransportDisconnect);
        assert_eq!(release.events, [KeyEvent::up(KeyUsage::A, 2)]);
        assert_eq!(
            session.output().events(),
            [KeyEvent::down(KeyUsage::A, 1), KeyEvent::up(KeyUsage::A, 2)]
        );
        assert!(session.pressed().is_empty());
        assert_eq!(session.output().release_all_calls(), 2);
        assert_eq!(
            session.tap(lease, KeyUsage::B, 3),
            Err(OutputSafetyError::Gate(OutputGateError::StaleLease))
        );

        session
            .source_connected(lost_source)
            .expect("lost source belongs to two-hand topology");
        let replacement = session
            .acquire_run_ownership()
            .expect("both reconnected sources permit a fresh lease");
        assert_ne!(replacement, lease);
        session
            .tap(replacement, KeyUsage::B, 4)
            .expect("the replacement lease restores output from neutral");
    }
}

#[test]
fn run_revocation_releases_without_claiming_desktop_focus_control() {
    let (mut session, first_lease) = armed_recording_session();
    session
        .apply(first_lease, KeyEvent::down(KeyUsage::Space, 10))
        .expect("the first run lease accepts a transition");
    let release = session
        .revoke_run_ownership(first_lease, 11)
        .expect("matching run revocation forces neutral");

    assert_eq!(release.reason, FaultReason::RunOwnershipRevoked);
    assert_eq!(release.events, [KeyEvent::up(KeyUsage::Space, 11)]);
    assert_eq!(
        session.tap(first_lease, KeyUsage::A, 12),
        Err(OutputSafetyError::Gate(OutputGateError::StaleLease))
    );

    let second_lease = session
        .acquire_run_ownership()
        .expect("a new run may acquire a new lease from neutral");
    assert_ne!(second_lease, first_lease);
    assert_eq!(
        session.revoke_run_ownership(first_lease, 13),
        Err(OutputSafetyError::Gate(OutputGateError::StaleLease))
    );
    session
        .tap(second_lease, KeyUsage::B, 14)
        .expect("a stale run lease cannot revoke the current lease");
}

#[test]
fn interrupt_and_terminate_shutdowns_release_after_continuous_dual_source_output() {
    const TAP_PAIRS_BEFORE_SHUTDOWN: u64 = 10_000;
    let usages = all_slots()
        .iter()
        .filter_map(|slot| slot.usage)
        .collect::<Vec<_>>();

    for (signal, reason) in [
        (ShutdownSignal::Interrupt, FaultReason::ProcessInterrupt),
        (ShutdownSignal::Terminate, FaultReason::ProcessTerminate),
    ] {
        let (mut session, lease) = armed_recording_session();
        for (pair, usage) in (0..TAP_PAIRS_BEFORE_SHUTDOWN).zip(usages.iter().copied().cycle()) {
            session
                .tap(lease, usage, pair.saturating_mul(2))
                .expect("continuous output keeps accepting exact Core-28 pairs");
        }
        let held_at_us = TAP_PAIRS_BEFORE_SHUTDOWN.saturating_mul(2);
        session
            .apply(lease, KeyEvent::down(KeyUsage::Backspace, held_at_us))
            .expect("continuous dual-source output accepts the transition");

        let release = session
            .shutdown(signal, held_at_us.saturating_add(1))
            .expect("planned signal shutdown forces neutral");

        assert_eq!(release.reason, reason);
        assert_eq!(
            release.events,
            [KeyEvent::up(
                KeyUsage::Backspace,
                held_at_us.saturating_add(1)
            )]
        );
        assert_eq!(
            session.output().events().len(),
            usize::try_from(
                TAP_PAIRS_BEFORE_SHUTDOWN
                    .saturating_mul(2)
                    .saturating_add(2)
            )
            .expect("continuous test event count fits usize")
        );
        assert!(session.pressed().is_empty());
        assert_eq!(session.output().release_all_calls(), 2);
        assert_eq!(
            session.acquire_run_ownership(),
            Err(OutputGateError::SessionStopped)
        );
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InjectedOutputError {
    Startup,
    Transition,
}

#[derive(Debug)]
struct FaultInjectingOutput {
    fail_startup: bool,
    fail_at_attempt: usize,
    emit_attempts: usize,
    pressed: std::collections::BTreeSet<KeyUsage>,
    release_all_calls: u64,
}

impl FaultInjectingOutput {
    fn new(fail_at_attempt: usize) -> Self {
        Self {
            fail_startup: false,
            fail_at_attempt,
            emit_attempts: 0,
            pressed: std::collections::BTreeSet::new(),
            release_all_calls: 0,
        }
    }
}

impl OutputAdapter for FaultInjectingOutput {
    type Error = InjectedOutputError;

    fn emit(
        &mut self,
        event: KeyEvent,
    ) -> Result<invisible_keyboard::output::OutputReceipt, Self::Error> {
        let attempt = self.emit_attempts;
        self.emit_attempts += 1;
        if attempt == self.fail_at_attempt {
            return Err(InjectedOutputError::Transition);
        }
        match event.state {
            EventState::Down => {
                self.pressed.insert(event.usage);
            }
            EventState::Up => {
                self.pressed.remove(&event.usage);
            }
        }
        Ok(invisible_keyboard::output::OutputReceipt {
            event,
            ordinal: u64::try_from(attempt).expect("test ordinal fits u64"),
        })
    }

    fn release_all(&mut self) -> Result<(), Self::Error> {
        if self.fail_startup && self.release_all_calls == 0 {
            return Err(InjectedOutputError::Startup);
        }
        self.pressed.clear();
        self.release_all_calls += 1;
        Ok(())
    }
}

#[test]
fn output_session_fails_closed_when_startup_neutralization_fails() {
    let output = FaultInjectingOutput {
        fail_startup: true,
        fail_at_attempt: usize::MAX,
        emit_attempts: 0,
        pressed: [KeyUsage::A].into_iter().collect(),
        release_all_calls: 0,
    };

    assert!(matches!(
        OutputSafetySession::start(output, RequiredSourceTopology::One(Hand::Left)),
        Err(InjectedOutputError::Startup)
    ));
}

#[test]
fn output_failure_on_either_edge_recovers_neutral_and_revokes_the_lease() {
    for fail_at_attempt in [0, 1] {
        let mut session = OutputSafetySession::start(
            FaultInjectingOutput::new(fail_at_attempt),
            RequiredSourceTopology::TwoHands,
        )
        .expect("fault adapter permits neutral startup");
        session
            .source_connected(Hand::Left)
            .expect("left belongs to two-hand topology");
        session
            .source_connected(Hand::Right)
            .expect("right belongs to two-hand topology");
        let lease = session
            .acquire_run_ownership()
            .expect("both sources are live");

        let failure = if fail_at_attempt == 0 {
            session.apply(lease, KeyEvent::down(KeyUsage::A, 1))
        } else {
            session
                .apply(lease, KeyEvent::down(KeyUsage::A, 1))
                .expect("down succeeds before injected up failure");
            session.apply(lease, KeyEvent::up(KeyUsage::A, 2))
        }
        .expect_err("the selected transition must fail");

        assert!(matches!(
            failure,
            OutputSafetyError::Adapter {
                cause: InjectedOutputError::Transition,
                recovery_error: None,
                ..
            }
        ));
        assert!(session.pressed().is_empty());
        assert!(session.output().pressed.is_empty());
        assert_eq!(session.output().release_all_calls, 2);
        assert!(!session.is_run_owned());
        assert_eq!(
            session.tap(lease, KeyUsage::B, 3),
            Err(OutputSafetyError::Gate(OutputGateError::SessionStopped))
        );
        assert_eq!(
            session.acquire_run_ownership(),
            Err(OutputGateError::SessionStopped)
        );
    }
}

#[test]
fn a_fresh_session_starts_neutral_and_rejects_the_previous_session_lease() {
    let mut first = OutputSafetySession::start(
        FaultInjectingOutput::new(usize::MAX),
        RequiredSourceTopology::TwoHands,
    )
    .expect("tracking output reaches neutral startup");
    first
        .source_connected(Hand::Left)
        .expect("left belongs to two-hand topology");
    first
        .source_connected(Hand::Right)
        .expect("right belongs to two-hand topology");
    let old_lease = first
        .acquire_run_ownership()
        .expect("the first session arms");
    first
        .apply(old_lease, KeyEvent::down(KeyUsage::A, 1))
        .expect("the first session records a held transition");
    assert_eq!(first.output().pressed, [KeyUsage::A].into_iter().collect());

    let mut restarted =
        OutputSafetySession::start(first.into_output(), RequiredSourceTopology::TwoHands)
            .expect("restart independently releases adapter state");
    assert!(restarted.pressed().is_empty());
    assert!(restarted.output().pressed.is_empty());
    assert_eq!(restarted.output().release_all_calls, 2);
    restarted
        .source_connected(Hand::Left)
        .expect("left belongs to two-hand topology");
    restarted
        .source_connected(Hand::Right)
        .expect("right belongs to two-hand topology");
    let new_lease = restarted
        .acquire_run_ownership()
        .expect("the restarted session arms only after both sources return");
    assert_ne!(new_lease, old_lease);
    assert_eq!(
        restarted.tap(old_lease, KeyUsage::B, 2),
        Err(OutputSafetyError::Gate(OutputGateError::StaleLease))
    );
    restarted
        .tap(new_lease, KeyUsage::B, 3)
        .expect("the fresh lease starts from neutral");
}

#[test]
fn host_owned_transitions_remain_explicit_system_test_gates() {
    assert_eq!(
        OUTPUT_SYSTEM_TEST_GATES,
        [
            OutputSystemTestGate::HostSuspendResume,
            OutputSystemTestGate::DesktopFocusRouting,
            OutputSystemTestGate::RecoverableFaultApplicationAllUpDeadline,
            OutputSystemTestGate::AbruptProcessDeathAllUpDeadline,
        ]
    );
}
