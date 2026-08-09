use std::{
    collections::BTreeSet,
    io::{Read, Write},
    net::TcpStream,
    sync::{Arc, Condvar, Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};

use invisible_keyboard::{
    calibration::{
        BlockId, CalibrationDomain, CalibrationFailure, CalibrationPhase, KeyboardPrecheckMetrics,
        LockedArtifactIdentity,
    },
    calibration_app::{
        CalibrationAction, CalibrationApp, CalibrationHttpServer, EvidenceMode,
        HardwareCalibrationEvent, KeyboardLayoutDecision, KeyboardLayoutDecisionState,
        ResultAcknowledgementState, calibration_failure_code,
    },
    labels::MonotonicHostClock,
    layout::{all_slots, keyboard_warm_start_eligible},
    validation::{
        PredictionClass, TruthClass, ValidationMatrix, ValidationReport, ValidationReportInput,
    },
};

#[test]
fn calibration_action_uses_one_typed_kebab_case_wire_vocabulary() {
    for (wire_name, expected) in [
        ("acknowledge-result", CalibrationAction::AcknowledgeResult),
        (
            "surface-instruction-rendered",
            CalibrationAction::SurfaceInstructionRendered,
        ),
        ("confirm-us-qwerty", CalibrationAction::ConfirmUsQwerty),
        ("reject-us-qwerty", CalibrationAction::RejectUsQwerty),
        (
            "advance-simulated-replay",
            CalibrationAction::AdvanceSimulatedReplay,
        ),
        ("pause", CalibrationAction::Pause),
        ("resume", CalibrationAction::Resume),
        ("abort", CalibrationAction::Abort),
        ("heartbeat", CalibrationAction::Heartbeat),
        ("visibility-hidden", CalibrationAction::VisibilityHidden),
        ("blur", CalibrationAction::Blur),
        ("pagehide", CalibrationAction::Pagehide),
        ("beforeunload", CalibrationAction::Beforeunload),
        ("offline", CalibrationAction::Offline),
    ] {
        assert_eq!(
            serde_json::from_str::<CalibrationAction>(&format!("\"{wire_name}\""))
                .expect("known action wire name is valid JSON"),
            expected
        );
    }

    assert_eq!(
        serde_json::from_str::<CalibrationAction>("\"future-action\"")
            .expect("an unknown string reaches the typed diagnostic variant"),
        CalibrationAction::Unknown("future-action".to_owned())
    );
}

#[test]
fn simulated_replay_visits_all_seven_screens_and_never_claims_hardware() {
    let app = CalibrationApp::new(EvidenceMode::SimulatedReplay);
    let mut screens = BTreeSet::new();
    let mut previous_revision = 0;
    let mut saw_surface_target = false;

    for _ in 0..12 {
        let snapshot = app.snapshot().expect("snapshot is available");
        assert!(snapshot.revision >= previous_revision);
        previous_revision = snapshot.revision;
        screens.insert(snapshot.screen.clone());
        if snapshot.phase == "surface-collect" {
            assert_eq!(snapshot.surface_target, "Tap Q");
            assert_eq!(snapshot.surface_position.as_deref(), Some("Q"));
            saw_surface_target = true;
        }
        if snapshot.heldout_validation_passed {
            break;
        }
        app.advance_simulated_replay()
            .expect("fixture advances through the real coordinator");
    }

    assert_eq!(
        screens,
        [
            "connect",
            "identify-keyboard",
            "keyboard-typing",
            "neutral-pose",
            "sensor-check",
            "surface-transfer",
            "validation-result",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    );
    assert!(saw_surface_target);
    let final_snapshot = app.snapshot().expect("final snapshot is available");
    assert!(final_snapshot.heldout_validation_passed);
    assert_eq!(final_snapshot.failure_cause, None);
    assert_eq!(final_snapshot.result_acknowledgement, "unavailable");
    assert_eq!(final_snapshot.evidence, "simulated-replay");
    assert!(final_snapshot.limitation.contains("does not establish"));
}

#[test]
fn frontend_contains_the_accessible_seven_screen_contract() {
    let html = invisible_keyboard::calibration_app::frontend_html("test-token");

    for screen in [
        "connect",
        "identify-keyboard",
        "sensor-check",
        "neutral-pose",
        "keyboard-typing",
        "surface-transfer",
        "validation-result",
    ] {
        assert!(html.contains(&format!("id=\"screen-{screen}\"")));
    }
    assert!(html.contains("aria-live=\"polite\""));
    assert!(html.contains("Setup progress"));
    assert!(html.contains("No keyboard angle or switch measurements"));
    assert!(html.contains("id=\"keyboard-precheck\""));
    assert!(html.contains("id=\"keyboard-accuracy\""));
    assert!(html.contains("id=\"surface-layout\""));
    assert!(html.contains("id=\"elapsed-seconds\""));
    assert!(html.contains("aria-label=\"Core-28 on a 34-position physical grid\""));
    assert!(html.contains("Reviewing this map is not scored."));
    assert!(html.contains("Exact Core-28 finger ownership on the 34-position grid"));
    assert!(html.contains("Six reserved positions are deliberate negative examples"));
    assert!(html.contains("They emit no key."));
    assert!(html.contains("Keyboard targets completed"));
    assert!(html.contains("Training targets completed"));
    assert!(html.contains("id=\"surface-stage\""));
    assert!(html.contains("Held-out validation checks the locked Core-28 key outputs"));
    assert!(html.contains("Plan for 10-20 minutes"));
    assert!(html.contains("fails closed at 20 minutes"));
    assert!(html.contains("Showing or acknowledging the cue does not record contact."));
    assert!(html.contains("The system cannot detect removal, movement, or loosening."));
    assert!(html.contains("data-action=\"acknowledge-result\""));
    assert_eq!(html.matches("data-position=\"").count(), all_slots().len());
    for slot in all_slots() {
        assert!(html.contains(&format!("data-position=\"{}\"", slot.label)));
        let hand = format!("{:?}", slot.position.hand).to_ascii_lowercase();
        let finger = format!("{:?}", slot.position.finger).to_ascii_lowercase();
        assert!(html.contains(&format!("{hand} {finger}")));
    }
    assert!(html.contains("No evidence-backed keyboard check has run."));
    let sensor_step = html
        .find("Step 2 of 7")
        .expect("sensor check is the second screen");
    let keyboard_step = html
        .find("Step 3 of 7")
        .expect("keyboard identification is the third screen");
    assert!(sensor_step < keyboard_step);
    assert!(!html.contains("100% calibrated"));
    assert!(!html.contains("current_mount"));
    assert!(!html.contains("readiness"));
    assert!(!html.contains("unsupported-for-current-mount"));
    assert!(!html.contains("https://"));
    assert!(!html.contains("http://"));
}

#[test]
fn rejected_keyboard_evidence_rotates_equal_score_prompts() {
    use invisible_keyboard::calibration::RejectedExampleReason;

    let app = CalibrationApp::new(EvidenceMode::Hardware);
    app.apply_hardware_event(HardwareCalibrationEvent::Connected)
        .expect("hardware connects");
    app.apply_hardware_event(HardwareCalibrationEvent::HardwareChecked)
        .expect("sensor check passes");
    let identified = app
        .apply_hardware_event(HardwareCalibrationEvent::KeyboardDeviceIdentified {
            device: "Fixture keyboard".to_owned(),
        })
        .expect("keyboard device is identified");
    let lease = app
        .decide_keyboard_layout(
            KeyboardLayoutDecision::ConfirmUsQwerty,
            identified
                .keyboard_layout_instruction_revision
                .expect("layout instruction is revision-bound"),
            10,
        )
        .expect("explicit layout confirmation is accepted");
    assert_eq!(lease.capture_lease_revision, None);
    let lease = app
        .activate_confirmed_layout_capture(
            lease
                .keyboard_layout_confirmation_revision
                .expect("confirmation preserves the instruction revision"),
        )
        .expect("runner arms capture after persisting the receipt")
        .capture_lease_revision
        .expect("active capture has a lease revision");
    let initial = app
        .apply_hardware_event(HardwareCalibrationEvent::NeutralAccepted)
        .expect("neutral evidence is accepted");
    let rotated = app
        .apply_hardware_event(HardwareCalibrationEvent::ExampleRejected {
            reason: RejectedExampleReason::Repeat,
            capture_lease_revision: lease,
        })
        .expect("rejected evidence is retained");

    assert_ne!(initial.prompt, rotated.prompt);
}

#[test]
fn hardware_worker_events_drive_the_same_finite_seven_screen_state_machine() {
    let app = CalibrationApp::new(EvidenceMode::Hardware);
    assert!(app.advance_simulated_replay().is_err());
    app.apply_hardware_event(HardwareCalibrationEvent::Connected)
        .expect("hardware connects");
    app.apply_hardware_event(HardwareCalibrationEvent::HardwareChecked)
        .expect("recorded sensor gate passes");
    let identified = app
        .apply_hardware_event(HardwareCalibrationEvent::KeyboardDeviceIdentified {
            device: "Fixture USB Keyboard (/dev/input/event9)".to_owned(),
        })
        .expect("selected keyboard device is frozen");
    assert!(identified.keyboard_device.contains("event9"));
    assert_eq!(identified.keyboard_layout_confirmation, "awaiting-user");
    let decided = app
        .decide_keyboard_layout(
            KeyboardLayoutDecision::ConfirmUsQwerty,
            identified
                .keyboard_layout_instruction_revision
                .expect("layout instruction is revision-bound"),
            20,
        )
        .expect("explicit layout confirmation is recorded");
    let lease = app
        .activate_confirmed_layout_capture(
            decided
                .keyboard_layout_confirmation_revision
                .expect("confirmation preserves the instruction revision"),
        )
        .expect("runner arms capture after persisting the receipt")
        .capture_lease_revision
        .expect("active capture has a lease revision");
    let instruction = app
        .apply_hardware_event(HardwareCalibrationEvent::CaptureInstruction {
            keyboard_prompt: Some("Press A twice".to_owned()),
            surface_target: None,
            surface_position: None,
            surface_instruction_receipt_required: false,
            neutral_seconds: Some(3),
            capture_lease_revision: lease,
        })
        .expect("the service publishes an evidence-stage instruction");
    assert_eq!(instruction.prompt, "Press A twice");
    assert_eq!(instruction.neutral_seconds, 3);
    app.apply_hardware_event(HardwareCalibrationEvent::NeutralAccepted)
        .expect("neutral evidence is accepted");

    for slot in all_slots()
        .iter()
        .filter(|slot| keyboard_warm_start_eligible(slot.position))
    {
        for _ in 0..4 {
            app.apply_hardware_event(HardwareCalibrationEvent::ExampleAccepted {
                domain: CalibrationDomain::Keyboard,
                position: slot.position,
                block: BlockId::new(10),
                capture_lease_revision: lease,
            })
            .expect("first keyboard block advances coverage");
        }
        for _ in 0..4 {
            app.apply_hardware_event(HardwareCalibrationEvent::ExampleAccepted {
                domain: CalibrationDomain::Keyboard,
                position: slot.position,
                block: BlockId::new(11),
                capture_lease_revision: lease,
            })
            .expect("second keyboard block advances coverage");
        }
    }
    app.apply_hardware_event(HardwareCalibrationEvent::BeginKeyboardPrecheck(
        BlockId::new(20),
    ))
    .expect("keyboard validation block is disjoint");
    let precheck = app
        .apply_hardware_event(HardwareCalibrationEvent::KeyboardPrecheck {
            metrics: KeyboardPrecheckMetrics::new(9_500, 9_000).expect("metrics are valid"),
            evaluated_windows: 112,
            weak_positions: Vec::new(),
        })
        .expect("keyboard validation passes");
    assert_eq!(
        precheck.keyboard_intent_separation_basis_points,
        Some(9_500)
    );
    assert_eq!(precheck.keyboard_finger_agreement_basis_points, Some(9_000));
    assert_eq!(precheck.keyboard_precheck_evaluated_windows, 112);
    assert!(precheck.keyboard_weak_positions.is_empty());
    assert!(precheck.keyboard_accuracy.contains("not available"));

    for slot in all_slots() {
        for _ in 0..3 {
            app.apply_hardware_event(HardwareCalibrationEvent::ExampleAccepted {
                domain: CalibrationDomain::PassiveSurface,
                position: slot.position,
                block: BlockId::new(30),
                capture_lease_revision: lease,
            })
            .expect("surface evidence advances coverage");
        }
    }
    app.apply_hardware_event(HardwareCalibrationEvent::BeginArtifactFit(BlockId::new(40)))
        .expect("artifact fitting starts after finite coverage");
    app.apply_hardware_event(HardwareCalibrationEvent::ArtifactLocked(
        LockedArtifactIdentity::new([0xa1; 32], [0xb1; 32], vec![[0xc1; 32]])
            .expect("identity is complete"),
    ))
    .expect("artifact locks before validation");
    let heldout_instruction = app
        .apply_hardware_event(HardwareCalibrationEvent::CaptureInstruction {
            keyboard_prompt: None,
            surface_target: Some("Tap Q now.".to_owned()),
            surface_position: Some(all_slots()[0].position),
            surface_instruction_receipt_required: true,
            neutral_seconds: None,
            capture_lease_revision: lease,
        })
        .expect("held-out target is published on the visible surface screen");
    assert_eq!(heldout_instruction.phase, "surface-validate");
    assert_eq!(heldout_instruction.screen, "surface-transfer");
    let heldout_revision = heldout_instruction
        .surface_instruction_revision
        .expect("held-out target requires a render receipt");
    app.acknowledge_surface_instruction(heldout_revision, 123_456)
        .expect("visible held-out target can be acknowledged");
    app.take_surface_instruction_ack(heldout_revision)
        .expect("held-out receipt remains readable")
        .expect("held-out receipt is consumable");
    let validated = app
        .apply_hardware_event(HardwareCalibrationEvent::SurfaceValidation(
            passing_hardware_validation(),
        ))
        .expect("held-out report is accepted");
    assert!(validated.heldout_validation_passed);
    assert_eq!(validated.screen, "validation-result");
    assert_eq!(validated.evidence, "hardware");
}

#[test]
fn hardware_capture_cannot_resume_or_label_after_pause() {
    let (app, lease) = hardware_app_at_keyboard_collect();
    let paused = app
        .apply_action(
            CalibrationAction::Pause,
            app.snapshot().expect("state is available").revision,
            0,
        )
        .expect("capture pauses");
    assert_eq!(paused.capture_lease_revision, None);
    let resume_error = app
        .apply_action(CalibrationAction::Resume, paused.revision, 0)
        .expect_err("hardware capture requires a new finite attempt after pause");
    assert!(resume_error.to_string().contains("not allowed"));

    let position = all_slots()
        .iter()
        .find(|slot| slot.usage.is_some())
        .expect("Core-28 has an addressable position")
        .position;
    let error = app
        .apply_hardware_event(HardwareCalibrationEvent::ExampleAccepted {
            domain: CalibrationDomain::Keyboard,
            position,
            block: BlockId::new(10),
            capture_lease_revision: lease,
        })
        .expect_err("an expired lease cannot label evidence");
    assert!(error.to_string().contains("no active capture lease"));
    assert_eq!(
        app.snapshot()
            .expect("state remains readable")
            .keyboard_accepted,
        0
    );
}

#[test]
fn hidden_tab_expires_capture_and_blocks_further_labels() {
    let (app, lease) = hardware_app_at_keyboard_collect();
    let hidden = app
        .apply_action(CalibrationAction::VisibilityHidden, lease, 0)
        .expect("hidden-tab signal expires the matching lease");
    assert!(hidden.paused);
    assert_eq!(hidden.capture_lease_revision, None);
    assert!(
        hidden
            .last_error
            .as_deref()
            .is_some_and(|message| message.contains("restart the setup command"))
    );

    let position = all_slots()[0].position;
    let error = app
        .apply_hardware_event(HardwareCalibrationEvent::ExampleAccepted {
            domain: CalibrationDomain::Keyboard,
            position,
            block: BlockId::new(10),
            capture_lease_revision: lease,
        })
        .expect_err("hidden capture cannot keep labeling");
    assert!(error.to_string().contains("no active capture lease"));
}

#[test]
fn dropped_browser_watchdog_expires_capture_fail_closed() {
    let (app, lease) = hardware_app_at_keyboard_collect();
    let expired = app
        .expire_capture_lease(lease)
        .expect("service watchdog expires the abandoned lease");
    assert!(expired.paused);
    assert_eq!(expired.capture_lease_revision, None);
    assert!(expired.last_error.as_deref().is_some_and(|message| {
        message.contains("heartbeat") && message.contains("restart the setup command")
    }));

    let error = app
        .apply_hardware_event(HardwareCalibrationEvent::ExampleRejected {
            reason: invisible_keyboard::calibration::RejectedExampleReason::Repeat,
            capture_lease_revision: lease,
        })
        .expect_err("a dropped browser cannot keep labeling");
    assert!(error.to_string().contains("no active capture lease"));
}

#[test]
fn every_snapshot_visible_error_or_lease_loss_advances_the_revision() {
    let app = CalibrationApp::new(EvidenceMode::Hardware);
    app.apply_hardware_event(HardwareCalibrationEvent::Connected)
        .expect("hardware connects once");
    let before_error = app
        .snapshot()
        .expect("connected state is readable")
        .revision;
    app.apply_hardware_event(HardwareCalibrationEvent::Connected)
        .expect_err("a duplicate connect is rejected");
    let errored = app.snapshot().expect("error state is readable");
    assert!(errored.revision > before_error);
    assert!(errored.last_error.is_some());

    let (leased_app, lease) = hardware_app_at_keyboard_collect();
    let before_expiry = leased_app
        .snapshot()
        .expect("leased state is readable")
        .revision;
    thread::sleep(Duration::from_millis(5_050));
    leased_app
        .apply_hardware_event(HardwareCalibrationEvent::ExampleAccepted {
            domain: CalibrationDomain::Keyboard,
            position: all_slots()[0].position,
            block: BlockId::new(10),
            capture_lease_revision: lease,
        })
        .expect_err("evidence cannot use an expired lease");
    let expired = leased_app.snapshot().expect("expired state is readable");
    assert_eq!(expired.revision, before_expiry.saturating_add(1));
    assert!(expired.paused);
    assert_eq!(expired.capture_lease_revision, None);
    assert!(
        expired
            .last_error
            .as_deref()
            .is_some_and(|message| message.contains("heartbeat expired"))
    );
}

#[test]
fn a_surface_label_requires_the_browser_to_acknowledge_the_rendered_instruction() {
    let (app, lease) = hardware_app_at_keyboard_collect();
    let instruction = app
        .apply_hardware_event(HardwareCalibrationEvent::CaptureInstruction {
            keyboard_prompt: None,
            surface_target: Some("Tap Q now.".to_owned()),
            surface_position: Some(all_slots()[0].position),
            surface_instruction_receipt_required: true,
            neutral_seconds: None,
            capture_lease_revision: lease,
        })
        .expect("the service publishes a surface cue");
    let instruction_revision = instruction
        .surface_instruction_revision
        .expect("the rendered state names the pending instruction");
    assert_eq!(instruction_revision, instruction.revision);
    assert_eq!(instruction.surface_position.as_deref(), Some("Q"));
    assert_eq!(
        app.take_surface_instruction_ack(instruction_revision)
            .expect("an unacknowledged instruction is not ready"),
        None
    );

    let stale = app
        .acknowledge_surface_instruction(instruction_revision.saturating_sub(1), 100_000)
        .expect_err("a stale rendered state cannot acknowledge the instruction");
    assert!(stale.to_string().contains("not pending"));

    app.acknowledge_surface_instruction(instruction_revision, 123_456)
        .expect("the visible browser acknowledges its rendered instruction");
    let receipt = app
        .take_surface_instruction_ack(instruction_revision)
        .expect("the service consumes the exact instruction receipt")
        .expect("the acknowledged instruction has one receipt");
    assert_eq!(receipt.instruction_revision, instruction_revision);
    assert_eq!(receipt.acknowledged_host_time_ns, 123_456);
    assert_eq!(
        app.take_surface_instruction_ack(instruction_revision)
            .expect("an instruction receipt is single use"),
        None
    );
    assert_eq!(
        app.snapshot()
            .expect("consumed instruction state is readable")
            .surface_instruction_revision,
        None,
        "the browser can stop retrying after the runner consumes the receipt"
    );

    let replacement = app
        .apply_hardware_event(HardwareCalibrationEvent::CaptureInstruction {
            keyboard_prompt: None,
            surface_target: Some("Checking the locked artifact.".to_owned()),
            surface_position: None,
            surface_instruction_receipt_required: false,
            neutral_seconds: None,
            capture_lease_revision: lease,
        })
        .expect("runner can replace a consumed cue with a non-receipt instruction");
    assert_eq!(replacement.surface_instruction_revision, None);
    assert_eq!(replacement.surface_target, "Checking the locked artifact.");
    assert!(
        app.acknowledge_surface_instruction(instruction_revision, 999_999)
            .is_err(),
        "an old browser retry cannot acknowledge the replacement instruction"
    );
}

#[test]
fn elapsed_seconds_are_exposed_as_exact_revision_stable_state() {
    let app = CalibrationApp::new(EvidenceMode::Hardware);
    let initial = app.snapshot().expect("initial state is available");
    assert_eq!(initial.elapsed_seconds, 0);

    let elapsed = app
        .apply_hardware_event(HardwareCalibrationEvent::ElapsedSeconds(17))
        .expect("the hardware clock publishes exact whole seconds");
    assert_eq!(elapsed.elapsed_seconds, 17);

    let repeated = app
        .apply_hardware_event(HardwareCalibrationEvent::ElapsedSeconds(17))
        .expect("repeating the current clock value is idempotent");
    assert_eq!(repeated.elapsed_seconds, 17);
    assert_eq!(repeated.revision, elapsed.revision);

    app.observe_elapsed_seconds(18)
        .expect("the runner updates elapsed time without requesting a snapshot");
    let runner_elapsed = app.snapshot().expect("runner time is visible");
    assert_eq!(runner_elapsed.elapsed_seconds, 18);
    app.observe_elapsed_seconds(18)
        .expect("repeating runner time is an allocation-free no-op");
    assert_eq!(
        app.snapshot()
            .expect("repeated runner time is visible")
            .revision,
        runner_elapsed.revision
    );
}

#[test]
fn capture_lease_liveness_uses_the_exact_service_revision() {
    let (app, lease) = hardware_app_at_keyboard_collect();

    assert!(
        app.capture_lease_is_live(lease)
            .expect("current lease can be checked")
    );
    assert!(
        !app.capture_lease_is_live(lease.saturating_add(1))
            .expect("a different revision can be checked")
    );
    app.apply_action(CalibrationAction::VisibilityHidden, lease, 0)
        .expect("browser lifecycle signal expires the lease");
    assert!(
        !app.capture_lease_is_live(lease)
            .expect("expired lease can be checked")
    );
}

#[test]
fn hardware_lease_requires_one_persisted_layout_confirmation() {
    let app = CalibrationApp::new(EvidenceMode::Hardware);
    let error = app
        .apply_action(CalibrationAction::Resume, 0, 0)
        .expect_err("generic resume cannot start physical capture");
    assert!(error.to_string().contains("not allowed"));

    app.apply_hardware_event(HardwareCalibrationEvent::Connected)
        .expect("hardware connects");
    app.apply_hardware_event(HardwareCalibrationEvent::HardwareChecked)
        .expect("sensor check passes");
    let identified = app
        .apply_hardware_event(HardwareCalibrationEvent::KeyboardDeviceIdentified {
            device: "Fixture keyboard".to_owned(),
        })
        .expect("keyboard device is selected without asserting layout");
    assert_eq!(identified.capture_lease_revision, None);
    let instruction_revision = identified
        .keyboard_layout_instruction_revision
        .expect("layout prompt has a dedicated revision");
    app.decide_keyboard_layout(
        KeyboardLayoutDecision::ConfirmUsQwerty,
        instruction_revision,
        25,
    )
    .expect("layout decision is recorded without arming capture");
    let active = app
        .activate_confirmed_layout_capture(instruction_revision)
        .expect("runner arms capture after durable receipt binding");
    assert!(active.capture_lease_revision.is_some());
    assert!(
        app.activate_confirmed_layout_capture(instruction_revision)
            .is_err()
    );
}

#[test]
fn layout_confirmation_uses_its_dedicated_revision_across_unrelated_state_updates() {
    let (app, instruction_revision) = hardware_app_at_layout_decision();
    let updated = app
        .apply_hardware_event(HardwareCalibrationEvent::ElapsedSeconds(7))
        .expect("elapsed time advances the global state");
    assert!(updated.revision > instruction_revision);
    assert_eq!(
        updated.keyboard_layout_instruction_revision,
        Some(instruction_revision)
    );

    let stale = app
        .decide_keyboard_layout(
            KeyboardLayoutDecision::ConfirmUsQwerty,
            instruction_revision + 1,
            100,
        )
        .expect_err("a different dedicated instruction revision is rejected");
    assert!(stale.to_string().contains("not pending for this revision"));
    assert_eq!(
        app.keyboard_layout_decision_state()
            .expect("typed decision state remains readable"),
        KeyboardLayoutDecisionState::Pending {
            instruction_revision
        }
    );

    let confirmed = app
        .decide_keyboard_layout(
            KeyboardLayoutDecision::ConfirmUsQwerty,
            instruction_revision,
            123_456,
        )
        .expect("the still-visible dedicated instruction remains valid");
    assert_eq!(
        confirmed.keyboard_layout_confirmation_revision,
        Some(instruction_revision)
    );
    assert_eq!(
        confirmed.keyboard_layout_confirmation_host_time_ns,
        Some(123_456)
    );
}

#[test]
fn confirmed_layout_decision_is_idempotent_and_cannot_flip_to_rejection() {
    let (app, instruction_revision) = hardware_app_at_layout_decision();
    let first = app
        .decide_keyboard_layout(
            KeyboardLayoutDecision::ConfirmUsQwerty,
            instruction_revision,
            200,
        )
        .expect("the positive decision is recorded");
    let first_receipt = app
        .keyboard_layout_confirmation_receipt()
        .expect("receipt state is readable")
        .expect("positive decision has a receipt");

    let duplicate = app
        .decide_keyboard_layout(
            KeyboardLayoutDecision::ConfirmUsQwerty,
            instruction_revision,
            999,
        )
        .expect("a response-loss retry is a successful no-op");
    assert_eq!(duplicate.revision, first.revision);
    assert_eq!(
        app.keyboard_layout_confirmation_receipt()
            .expect("receipt state remains readable"),
        Some(first_receipt.clone())
    );
    assert_eq!(first_receipt.acknowledged_host_time_ns, 200);

    let conflict = app
        .decide_keyboard_layout(
            KeyboardLayoutDecision::Unsupported,
            instruction_revision,
            1_000,
        )
        .expect_err("a recorded yes cannot later become no");
    assert!(conflict.to_string().contains("cannot change"));
    assert_eq!(
        app.keyboard_layout_decision_state()
            .expect("typed decision remains readable"),
        KeyboardLayoutDecisionState::Confirmed(first_receipt)
    );
}

#[test]
fn rejected_layout_decision_is_typed_terminal_idempotent_and_irreversible() {
    let (app, instruction_revision) = hardware_app_at_layout_decision();
    let rejected = app
        .decide_keyboard_layout(
            KeyboardLayoutDecision::Unsupported,
            instruction_revision,
            300,
        )
        .expect("the negative decision is recorded");
    assert_eq!(rejected.phase, "calibration-failed");
    assert_eq!(
        rejected.failure_cause.as_deref(),
        Some("unsupported-layout")
    );
    assert!(rejected.paused);
    assert_eq!(rejected.capture_lease_revision, None);
    assert_eq!(
        app.keyboard_layout_decision_state()
            .expect("typed rejection is available to the runner"),
        KeyboardLayoutDecisionState::Rejected {
            instruction_revision,
            acknowledged_host_time_ns: 300,
        }
    );
    assert_eq!(
        app.control_state()
            .expect("typed terminal state is readable"),
        invisible_keyboard::calibration_app::CalibrationControlState {
            phase: CalibrationPhase::CalibrationFailed,
            failure: Some(CalibrationFailure::UnsupportedLayout),
            capture_lease_revision: None,
            paused: true,
        }
    );

    let duplicate = app
        .decide_keyboard_layout(
            KeyboardLayoutDecision::Unsupported,
            instruction_revision,
            999,
        )
        .expect("a repeated negative decision is a successful no-op");
    assert_eq!(duplicate.revision, rejected.revision);
    assert_eq!(
        duplicate.keyboard_layout_confirmation_host_time_ns,
        Some(300)
    );

    let conflict = app
        .decide_keyboard_layout(
            KeyboardLayoutDecision::ConfirmUsQwerty,
            instruction_revision,
            1_000,
        )
        .expect_err("a recorded no cannot later become yes");
    assert!(conflict.to_string().contains("cannot change"));
}

#[test]
fn calibration_failure_codes_are_stable_and_exhaustive() {
    assert_eq!(
        calibration_failure_code(CalibrationFailure::HardwareFault),
        "hardware-fault"
    );
    assert_eq!(
        calibration_failure_code(CalibrationFailure::UnsupportedLayout),
        "unsupported-layout"
    );
    assert_eq!(
        calibration_failure_code(CalibrationFailure::SurfaceValidation),
        "heldout-validation-failed"
    );
    assert_eq!(
        calibration_failure_code(CalibrationFailure::ValidationPlateau),
        "validation-plateau"
    );
    assert_eq!(
        calibration_failure_code(CalibrationFailure::ExampleCap),
        "example-cap"
    );
    assert_eq!(
        calibration_failure_code(CalibrationFailure::TimeCap),
        "time-cap"
    );

    let faulted = CalibrationApp::new(EvidenceMode::Hardware)
        .apply_hardware_event(HardwareCalibrationEvent::HardwareFault(
            "left sensor stream stopped".to_owned(),
        ))
        .expect("hardware fault creates a structured terminal snapshot");
    assert_eq!(faulted.phase, "calibration-failed");
    assert_eq!(faulted.failure_cause.as_deref(), Some("hardware-fault"));
    assert!(!faulted.heldout_validation_passed);
}

#[test]
fn a_published_saved_result_is_read_only_until_exact_acknowledgement() {
    let app = simulated_app_at_validation_passed();
    let published = app
        .publish_saved_result()
        .expect("the caller publishes only after saving the result");
    let acknowledgement_revision = published
        .result_acknowledgement_revision
        .expect("published result has a dedicated revision");
    assert_eq!(published.result_acknowledgement, "pending");
    assert_eq!(
        app.result_acknowledgement_state()
            .expect("typed acknowledgement state is readable"),
        ResultAcknowledgementState::Pending(acknowledgement_revision)
    );

    let mutation = app
        .apply_action(CalibrationAction::Abort, published.revision, 0)
        .expect_err("published result rejects every unrelated mutation");
    assert!(mutation.to_string().contains("read-only"));
    assert!(app.observe_elapsed_seconds(1_200).is_err());
    assert_eq!(
        app.snapshot().expect("result remains readable").phase,
        "validation-passed"
    );

    assert!(
        app.acknowledge_result(acknowledgement_revision.saturating_sub(1), 400)
            .is_err()
    );
    let acknowledged = app
        .acknowledge_result(acknowledgement_revision, 500)
        .expect("the exact saved result is acknowledged");
    assert_eq!(acknowledged.result_acknowledgement, "acknowledged");
    assert_eq!(acknowledged.result_acknowledgement_host_time_ns, Some(500));
    let duplicate = app
        .acknowledge_result(acknowledgement_revision, 999)
        .expect("an acknowledgement retry preserves the first receipt");
    assert_eq!(duplicate.revision, acknowledged.revision);
    assert_eq!(duplicate.result_acknowledgement_host_time_ns, Some(500));
}

#[test]
fn no_data_keyboard_precheck_is_explicitly_unknown() {
    let snapshot = CalibrationApp::new(EvidenceMode::Hardware)
        .snapshot()
        .expect("initial state is available");

    assert_eq!(snapshot.keyboard_coverage_basis_points, Some(0));
    assert_eq!(snapshot.keyboard_intent_separation_basis_points, None);
    assert_eq!(snapshot.keyboard_finger_agreement_basis_points, None);
    assert_eq!(snapshot.keyboard_precheck_evaluated_windows, 0);
    assert!(snapshot.keyboard_weak_positions.is_empty());
    assert_eq!(
        snapshot.keyboard_precheck,
        "No evidence-backed keyboard check has run."
    );
    assert_eq!(
        snapshot.keyboard_accuracy,
        "Key-identity accuracy is not available from the keyboard precheck."
    );
}

#[test]
fn abort_revokes_capture_and_cannot_leave_passed_validation() {
    let app = CalibrationApp::new(EvidenceMode::SimulatedReplay);
    loop {
        let snapshot = app.snapshot().expect("state is available");
        if snapshot.heldout_validation_passed {
            let aborted = app
                .apply_action(CalibrationAction::Abort, snapshot.revision, 0)
                .expect("abort finalizes the attempt");
            assert_eq!(aborted.phase, "aborted");
            assert!(!aborted.heldout_validation_passed);
            assert!(aborted.paused);
            assert_eq!(aborted.capture_lease_revision, None);
            break;
        }
        app.advance_simulated_replay()
            .expect("fixture advances to a terminal result");
    }
}

#[test]
fn hardware_abort_accepts_stale_rendered_revision_after_elapsed_progress() {
    let (app, lease) = hardware_app_at_keyboard_collect();
    let rendered = app.snapshot().expect("capture state is rendered");
    app.observe_elapsed_seconds(7)
        .expect("runner publishes elapsed progress");
    let progressed = app.snapshot().expect("elapsed progress remains readable");
    assert!(progressed.revision > rendered.revision);
    assert_eq!(progressed.capture_lease_revision, Some(lease));
    assert!(
        app.apply_action(CalibrationAction::Pause, rendered.revision, 0)
            .is_err()
    );
    app.apply_action(CalibrationAction::Heartbeat, lease, 0)
        .expect("unrelated progress leaves the capture lease valid");

    let aborted = app
        .apply_action(CalibrationAction::Abort, rendered.revision, 0)
        .expect("Stop setup is not conditional on the rendered state staying current");
    assert_eq!(aborted.phase, "aborted");
    assert!(aborted.paused);
    assert_eq!(aborted.capture_lease_revision, None);
    assert!(
        app.apply_action(CalibrationAction::Heartbeat, lease, 0)
            .is_err()
    );
    assert!(
        app.apply_hardware_event(HardwareCalibrationEvent::NeutralAccepted)
            .is_err()
    );
    assert_eq!(
        app.snapshot()
            .expect("stopped state remains readable")
            .phase,
        "aborted"
    );
}

#[test]
fn hardware_http_abort_accepts_stale_revision_only_with_loopback_authorization() {
    let (app, lease) = hardware_app_at_keyboard_collect();
    let rendered = app.snapshot().expect("capture state is rendered");
    let progressed = app
        .apply_hardware_event(HardwareCalibrationEvent::ElapsedSeconds(7))
        .expect("hardware progress advances the global revision");
    assert!(progressed.revision > rendered.revision);
    assert_eq!(progressed.capture_lease_revision, Some(lease));
    let server = CalibrationHttpServer::bind(app.clone(), 0).expect("server binds loopback");
    let port = server.local_addr().expect("server has an address").port();
    let token = server.token();
    let body = format!(
        "{{\"action\":\"abort\",\"revision\":{}}}",
        rendered.revision
    );
    let authorized = action_request(port, token, &body);
    for unauthorized in [
        action_request(port, "wrong-token", &body),
        authorized.replace(
            &format!("Origin: http://127.0.0.1:{port}"),
            "Origin: https://untrusted.example",
        ),
        authorized
            .replace(
                &format!("Host: 127.0.0.1:{port}"),
                "Host: untrusted.example",
            )
            .replace(
                &format!("Origin: http://127.0.0.1:{port}"),
                "Origin: http://untrusted.example",
            ),
    ] {
        let rejected = request_through_poll(&server, port, &unauthorized);
        assert!(rejected.starts_with("HTTP/1.1 4"));
        let unchanged = app
            .snapshot()
            .expect("unauthorized stop leaves capture intact");
        assert_eq!(unchanged.phase, rendered.phase);
        assert!(!unchanged.paused);
        assert_eq!(unchanged.capture_lease_revision, Some(lease));
    }
    let stale_pause = format!(
        "{{\"action\":\"pause\",\"revision\":{}}}",
        rendered.revision
    );
    let rejected = request_through_poll(&server, port, &action_request(port, token, &stale_pause));
    assert!(rejected.starts_with("HTTP/1.1 400"));

    let stopped = request_through_poll(&server, port, &authorized);
    assert!(stopped.starts_with("HTTP/1.1 200"));
    assert!(stopped.contains("\"phase\":\"aborted\""));
    assert!(stopped.contains("\"paused\":true"));
    assert!(stopped.contains("\"capture_lease_revision\":null"));
    assert!(
        app.apply_action(CalibrationAction::Heartbeat, lease, 0)
            .is_err()
    );
    assert!(
        app.apply_hardware_event(HardwareCalibrationEvent::NeutralAccepted)
            .is_err()
    );
    let stopped = app.snapshot().expect("stopped state remains readable");
    assert_eq!(stopped.phase, "aborted");
    assert!(stopped.paused);
    assert_eq!(stopped.capture_lease_revision, None);
}

#[test]
fn hardware_stop_preserves_terminal_failure_after_rendered_state_changes() {
    for hardware_fault in [false, true] {
        for stale_revision in [false, true] {
            let (app, _) = hardware_app_at_keyboard_collect();
            let rendered = app.snapshot().expect("active capture is rendered");
            let failed = app
                .apply_hardware_event(if hardware_fault {
                    HardwareCalibrationEvent::HardwareFault("left sensor stream stopped".to_owned())
                } else {
                    HardwareCalibrationEvent::ElapsedSeconds(1_200)
                })
                .expect("hardware failure or time cap terminates the attempt");
            assert!(failed.revision > rendered.revision);
            assert_eq!(
                failed.failure_cause.as_deref(),
                Some(if hardware_fault {
                    "hardware-fault"
                } else {
                    "time-cap"
                })
            );
            let supplied_revision = if stale_revision {
                rendered.revision
            } else {
                failed.revision
            };
            let stopped = app
                .apply_action(CalibrationAction::Abort, supplied_revision, 0)
                .expect("Stop accepts both current and delayed authenticated intent");
            assert_eq!(stopped.phase, failed.phase);
            assert_eq!(stopped.failure_cause, failed.failure_cause);
            assert_eq!(stopped.hardware_health, failed.hardware_health);
            assert_eq!(stopped.last_error, failed.last_error);
            assert!(stopped.paused);
            assert_eq!(stopped.capture_lease_revision, None);
            assert!(
                app.apply_hardware_event(HardwareCalibrationEvent::NeutralAccepted)
                    .is_err()
            );
        }
    }
}

fn hardware_app_at_keyboard_collect() -> (CalibrationApp, u64) {
    let (app, instruction_revision) = hardware_app_at_layout_decision();
    app.decide_keyboard_layout(
        KeyboardLayoutDecision::ConfirmUsQwerty,
        instruction_revision,
        10,
    )
    .expect("layout confirmation is recorded");
    let lease = app
        .activate_confirmed_layout_capture(instruction_revision)
        .expect("runner arms capture after receipt persistence")
        .capture_lease_revision
        .expect("active capture has a lease");
    app.apply_hardware_event(HardwareCalibrationEvent::NeutralAccepted)
        .expect("neutral evidence advances to keyboard collection");
    (app, lease)
}

fn simulated_app_at_validation_passed() -> CalibrationApp {
    let app = CalibrationApp::new(EvidenceMode::SimulatedReplay);
    while !app
        .snapshot()
        .expect("simulated result remains readable")
        .heldout_validation_passed
    {
        app.advance_simulated_replay()
            .expect("simulated evidence reaches held-out validation");
    }
    app
}

fn hardware_app_at_layout_decision() -> (CalibrationApp, u64) {
    let app = CalibrationApp::new(EvidenceMode::Hardware);
    app.apply_hardware_event(HardwareCalibrationEvent::Connected)
        .expect("hardware connects");
    app.apply_hardware_event(HardwareCalibrationEvent::HardwareChecked)
        .expect("sensor check passes");
    let identified = app
        .apply_hardware_event(HardwareCalibrationEvent::KeyboardDeviceIdentified {
            device: "Fixture keyboard".to_owned(),
        })
        .expect("keyboard device is identified");
    let instruction_revision = identified
        .keyboard_layout_instruction_revision
        .expect("layout instruction is revision-bound");
    (app, instruction_revision)
}

fn passing_hardware_validation() -> ValidationReport {
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
    confusion.observe(TruthClass::Null, PredictionClass::Null);
    ValidationReport::new(ValidationReportInput {
        artifact_sha256: [0xa1; 32],
        acquisition_manifest_sha256: [0xb1; 32],
        evaluator_version: "hardware-worker-test-v1".to_owned(),
        session_sha256: vec![[0xd1; 32]],
        evidence_sha256: vec![[0xd2; 32]],
        block_ids: vec![40],
        confusion,
        ordinary_motion_exposure_ms: 60_000,
        key_state_neutral: true,
    })
    .expect("validation is structurally complete")
}

#[test]
fn loopback_http_requires_the_per_process_token_and_origin() {
    let app = CalibrationApp::new(EvidenceMode::SimulatedReplay);
    let server = CalibrationHttpServer::bind(app.clone(), 0).expect("server binds loopback");
    let address = server.local_addr().expect("server has an address");
    let token = server.token().to_owned();
    let worker = thread::spawn(move || server.serve_n_requests(8).expect("requests are served"));

    let root = request(address.port(), "GET / HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n");
    assert!(root.starts_with("HTTP/1.1 200"));
    assert!(root.contains("Content-Security-Policy:"));
    assert!(root.contains(&format!("content=\"{token}\"")));

    let javascript = request(
        address.port(),
        "GET /app.js HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
    );
    assert!(javascript.starts_with("HTTP/1.1 200"));
    for lifecycle_hook in [
        "visibilitychange",
        "blur",
        "pagehide",
        "beforeunload",
        "offline",
        "heartbeat",
        "keepalive",
        "capture_lease_revision",
        "event.preventDefault()",
        "surface-instruction-rendered",
        "surfaceInstructionAcknowledgementInFlightRevision",
        "requestAnimationFrame",
        "aria-current",
        "surface_position",
        "Physical capture is one fail-closed attempt",
        "acknowledge-result",
        "result_acknowledgement_revision",
        "heldout_validation_passed",
        "Held-out validation passed. The model and evidence were saved.",
        "Typing stopped and output is neutral. This wear-session confirmation is no longer active. Confirm again before starting a new live process. Revalidate first if either wearable moved.",
        "pageNavigationKey",
        "const ACTIVE_CAPTURE_POLL_MS = 100",
        "const INACTIVE_POLL_MS = 1000",
        "if (state.revision === renderedRevision)",
        "applyState(await api('/api/state'))",
        "scheduleRefresh();",
        "state.screen !== 'surface-transfer'",
        "surfaceTransferScreen.hidden",
        "surfacePrompt.textContent !== state.surface_target",
        "if (state.surface_instruction_revision === null)",
    ] {
        assert!(javascript.contains(lifecycle_hook));
    }
    for removed_contract in [
        "current_mount",
        "readiness",
        "unsupported-for-current-mount",
        "needs-hardware-attention",
        "Revalidate before starting again.",
    ] {
        assert!(!javascript.contains(removed_contract));
    }
    let post_ack = javascript
        .find("postAction('surface-instruction-rendered'")
        .expect("the browser posts a render acknowledgement");
    let mark_acknowledged = javascript
        .find("lastAcknowledgedSurfaceInstructionRevision = instructionRevision")
        .expect("the browser records successful acknowledgement");
    assert!(post_ack < mark_acknowledged);
    assert!(javascript.contains("surfaceInstructionAcknowledgementInFlightRevision !== null"));
    assert!(javascript.contains("finishSurfaceInstructionAcknowledgement(instructionRevision)"));

    let stylesheet = request(
        address.port(),
        "GET /style.css HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
    );
    assert!(stylesheet.starts_with("HTTP/1.1 200"));
    assert!(stylesheet.contains("background: #000"));
    assert!(stylesheet.contains(":focus-visible"));
    assert!(stylesheet.contains("prefers-reduced-motion"));
    assert!(!stylesheet.contains("@keyframes"));
    assert!(!stylesheet.contains("transition: all"));

    let forbidden = request(
        address.port(),
        "GET /api/state HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
    );
    assert!(forbidden.starts_with("HTTP/1.1 403"));

    let body = r#"{"action":"advance-simulated-replay","revision":0}"#;
    let wrong_origin = request(
        address.port(),
        &format!(
            "POST /api/action HTTP/1.1\r\nHost: 127.0.0.1\r\nOrigin: https://attacker.invalid\r\nX-Calibration-Token: {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        ),
    );
    assert!(wrong_origin.starts_with("HTTP/1.1 403"));

    let accepted = request(
        address.port(),
        &format!(
            "POST /api/action HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nOrigin: http://127.0.0.1:{}\r\nX-Calibration-Token: {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            address.port(),
            address.port(),
            body.len()
        ),
    );
    assert!(accepted.starts_with("HTTP/1.1 200"));

    let unknown_body = r#"{"action":"future-action","revision":1}"#;
    let unknown = request(
        address.port(),
        &action_request(address.port(), &token, unknown_body),
    );
    assert!(unknown.starts_with("HTTP/1.1 400"));
    assert!(unknown.ends_with("\r\n\r\nunknown calibration action future-action"));

    let state = request(
        address.port(),
        &format!(
            "GET /api/state HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Calibration-Token: {token}\r\n\r\n"
        ),
    );
    assert!(state.starts_with("HTTP/1.1 200"));
    assert!(state.contains("\"screen\":\"sensor-check\""));
    assert!(state.contains("\"elapsed_seconds\":0"));

    worker.join().expect("server thread exits");
}

#[test]
fn published_result_http_allows_reads_and_only_the_acknowledgement_action() {
    let app = simulated_app_at_validation_passed();
    let published = app
        .publish_saved_result()
        .expect("saved result becomes available for review");
    let acknowledgement_revision = published
        .result_acknowledgement_revision
        .expect("saved result has a dedicated acknowledgement revision");
    let clock = DeterministicClock::new(700);
    let server = CalibrationHttpServer::bind_with_host_clock(app.clone(), 0, clock)
        .expect("result server binds loopback");
    let address = server.local_addr().expect("server has an address");
    let token = server.token().to_owned();

    let state = request_through_poll(
        &server,
        address.port(),
        &format!(
            "GET /api/state HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Calibration-Token: {token}\r\n\r\n"
        ),
    );
    assert!(state.starts_with("HTTP/1.1 200"));
    assert!(state.contains("\"phase\":\"validation-passed\""));
    assert!(state.contains("\"result_acknowledgement\":\"pending\""));

    let abort_body = format!(
        "{{\"action\":\"abort\",\"revision\":{}}}",
        published.revision
    );
    let rejected = request_through_poll(
        &server,
        address.port(),
        &action_request(address.port(), &token, &abort_body),
    );
    assert!(rejected.starts_with("HTTP/1.1 400"));
    assert!(rejected.contains("read-only"));

    let acknowledge_body =
        format!("{{\"action\":\"acknowledge-result\",\"revision\":{acknowledgement_revision}}}");
    let acknowledged = request_through_poll(
        &server,
        address.port(),
        &action_request(address.port(), &token, &acknowledge_body),
    );
    assert!(acknowledged.starts_with("HTTP/1.1 200"));
    assert!(acknowledged.contains("\"result_acknowledgement\":\"acknowledged\""));
    assert_eq!(
        app.result_acknowledgement_receipt()
            .expect("acknowledgement receipt remains readable")
            .expect("HTTP action records the receipt")
            .acknowledged_host_time_ns,
        700
    );
}

#[test]
fn http_surface_acknowledgement_preserves_first_accept_time_across_retries() {
    let (app, lease) = hardware_app_at_keyboard_collect();
    let instruction = app
        .apply_hardware_event(HardwareCalibrationEvent::CaptureInstruction {
            keyboard_prompt: None,
            surface_target: Some("Tap Q now.".to_owned()),
            surface_position: Some(all_slots()[0].position),
            surface_instruction_receipt_required: true,
            neutral_seconds: None,
            capture_lease_revision: lease,
        })
        .expect("the service publishes a surface cue");
    let instruction_revision = instruction
        .surface_instruction_revision
        .expect("the surface cue requires a render receipt");
    let state_revision_before_ack = app
        .apply_hardware_event(HardwareCalibrationEvent::ElapsedSeconds(7))
        .expect("elapsed state can advance while the cue is pending")
        .revision;
    assert!(state_revision_before_ack > instruction_revision);
    let clock = DeterministicClock::new(10);
    let server = CalibrationHttpServer::bind_with_host_clock(app.clone(), 0, clock.clone())
        .expect("server binds with the runner-compatible clock");
    let address = server.local_addr().expect("server has an address");
    let token = server.token().to_owned();

    let stale_body = format!(
        "{{\"action\":\"surface-instruction-rendered\",\"revision\":{}}}",
        instruction_revision.saturating_sub(1)
    );
    clock.set(100);
    let stale = request_through_poll(
        &server,
        address.port(),
        &action_request(address.port(), &token, &stale_body),
    );
    assert!(stale.starts_with("HTTP/1.1 400"));
    assert_eq!(
        app.take_surface_instruction_ack(instruction_revision)
            .expect("stale acknowledgement leaves the cue pending"),
        None
    );

    let body = format!(
        "{{\"action\":\"surface-instruction-rendered\",\"revision\":{instruction_revision}}}"
    );
    clock.set(123_456);
    let mut delayed_stream =
        TcpStream::connect(("127.0.0.1", address.port())).expect("delayed request connects");
    delayed_stream
        .write_all(b"POST /api/action HTTP/1.1\r\n")
        .expect("the request line reaches the accept queue");
    let accepted = thread::scope(|scope| {
        let worker = scope.spawn(|| server.poll_once().expect("server accepts the request"));
        clock.wait_for_reads(2);
        clock.set(999_999);
        let request = action_request(address.port(), &token, &body);
        let (_, remaining_request) = request
            .split_once("\r\n")
            .expect("the HTTP request has a request line");
        delayed_stream
            .write_all(remaining_request.as_bytes())
            .expect("the rest of the delayed request writes");
        delayed_stream
            .shutdown(std::net::Shutdown::Write)
            .expect("the delayed request ends");
        let mut response = String::new();
        delayed_stream
            .read_to_string(&mut response)
            .expect("the delayed response reads");
        assert!(worker.join().expect("server poll thread exits"));
        response
    });
    assert!(accepted.starts_with("HTTP/1.1 200"));
    assert_eq!(
        app.snapshot()
            .expect("acknowledged state remains readable")
            .revision,
        state_revision_before_ack,
        "receipt acceptance does not create a second instruction identity"
    );

    clock.set(999_999);
    let duplicate = request_through_poll(
        &server,
        address.port(),
        &action_request(address.port(), &token, &body),
    );
    assert!(duplicate.starts_with("HTTP/1.1 200"));
    assert_eq!(
        app.snapshot()
            .expect("duplicate acknowledgement leaves state readable")
            .revision,
        state_revision_before_ack,
        "duplicate acknowledgement is a state-preserving success"
    );

    clock.set(5_000_000);
    let receipt = app
        .take_surface_instruction_ack(instruction_revision)
        .expect("the runner polls for the receipt later")
        .expect("the accepted request produced a receipt");
    assert_eq!(receipt.instruction_revision, instruction_revision);
    assert_eq!(receipt.acknowledged_host_time_ns, 123_456);
    let consumed_revision = app
        .snapshot()
        .expect("consumed receipt state remains readable")
        .revision;

    clock.set(9_000_000);
    let retry_after_consumption = request_through_poll(
        &server,
        address.port(),
        &action_request(address.port(), &token, &body),
    );
    assert!(retry_after_consumption.starts_with("HTTP/1.1 200"));
    assert_eq!(
        app.snapshot()
            .expect("retried acknowledgement leaves state readable")
            .revision,
        consumed_revision,
        "a response-loss retry cannot recreate the consumed receipt"
    );
    assert_eq!(
        app.take_surface_instruction_ack(instruction_revision)
            .expect("an idempotent retry cannot create a second receipt"),
        None
    );
}

#[test]
fn http_layout_confirmation_uses_connection_accept_time() {
    let (app, instruction_revision) = hardware_app_at_layout_decision();
    let clock = DeterministicClock::new(123_456);
    let server = CalibrationHttpServer::bind_with_host_clock(app.clone(), 0, clock.clone())
        .expect("server binds with the capture clock");
    let address = server.local_addr().expect("server has an address");
    let token = server.token().to_owned();
    let body = format!("{{\"action\":\"confirm-us-qwerty\",\"revision\":{instruction_revision}}}");
    let request = action_request(address.port(), &token, &body);
    let (_, remaining_request) = request
        .split_once("\r\n")
        .expect("the HTTP request has a request line");
    let mut delayed_stream =
        TcpStream::connect(("127.0.0.1", address.port())).expect("delayed request connects");
    delayed_stream
        .write_all(b"POST /api/action HTTP/1.1\r\n")
        .expect("the request line reaches the accept queue");

    let response = thread::scope(|scope| {
        let worker = scope.spawn(|| server.poll_once().expect("server accepts the request"));
        clock.wait_for_reads(1);
        clock.set(999_999);
        delayed_stream
            .write_all(remaining_request.as_bytes())
            .expect("the rest of the delayed request writes");
        delayed_stream
            .shutdown(std::net::Shutdown::Write)
            .expect("the delayed request ends");
        let mut response = String::new();
        delayed_stream
            .read_to_string(&mut response)
            .expect("the delayed response reads");
        assert!(worker.join().expect("server poll thread exits"));
        response
    });

    assert!(response.starts_with("HTTP/1.1 200"));
    let receipt = app
        .keyboard_layout_confirmation_receipt()
        .expect("receipt state is readable")
        .expect("the HTTP decision produced a receipt");
    assert_eq!(receipt.instruction_revision, instruction_revision);
    assert_eq!(receipt.acknowledged_host_time_ns, 123_456);
}

#[test]
fn malformed_http_isolated_to_one_connection() {
    let app = CalibrationApp::new(EvidenceMode::SimulatedReplay);
    let server = CalibrationHttpServer::bind(app, 0).expect("server binds loopback");
    let address = server.local_addr().expect("server has an address");
    let token = server.token().to_owned();
    let worker = thread::spawn(move || server.serve_n_requests(2).expect("requests are served"));

    let malformed = request(address.port(), "BROKEN\r\n\r\n");
    assert!(malformed.starts_with("HTTP/1.1 400"));
    assert!(malformed.contains("malformed HTTP request: path is missing"));

    let state = request(
        address.port(),
        &format!(
            "GET /api/state HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Calibration-Token: {token}\r\n\r\n"
        ),
    );
    assert!(state.starts_with("HTTP/1.1 200"));
    assert!(state.contains("\"phase\":\"disconnected\""));

    worker.join().expect("server thread exits");
}

#[test]
fn stalled_http_connection_times_out_without_blocking_the_service() {
    let app = CalibrationApp::new(EvidenceMode::SimulatedReplay);
    let server = CalibrationHttpServer::bind(app, 0).expect("server binds loopback");
    let address = server.local_addr().expect("server has an address");
    let token = server.token().to_owned();
    let worker = thread::spawn(move || server.serve_n_requests(2).expect("requests are served"));

    let mut stalled = TcpStream::connect(address).expect("stalled client connects");
    stalled
        .set_read_timeout(Some(Duration::from_secs(4)))
        .expect("test read timeout applies");
    let mut timeout_response = String::new();
    stalled
        .read_to_string(&mut timeout_response)
        .expect("server closes the stalled request");
    assert!(timeout_response.starts_with("HTTP/1.1 408"));

    let state = request(
        address.port(),
        &format!(
            "GET /api/state HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Calibration-Token: {token}\r\n\r\n"
        ),
    );
    assert!(state.starts_with("HTTP/1.1 200"));

    worker.join().expect("server thread exits");
}

#[test]
fn progressing_http_headers_have_a_cumulative_deadline() {
    assert_progressing_http_request_times_out(
        b"POST /api/action HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Progress: ",
    );
}

#[test]
fn progressing_http_body_has_a_cumulative_deadline() {
    assert_progressing_http_request_times_out(
        b"POST /api/action HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 4096\r\n\r\n{",
    );
}

fn assert_progressing_http_request_times_out(prefix: &[u8]) {
    let (app, lease) = hardware_app_at_keyboard_collect();
    // The provenance clock deliberately never advances. Socket deadlines must
    // use real monotonic time, not the clock injected for browser receipts.
    let clock = DeterministicClock::new(123_456);
    let server = CalibrationHttpServer::bind_with_host_clock(app.clone(), 0, clock.clone())
        .expect("server binds with a frozen provenance clock");
    let address = server.local_addr().expect("server has an address");
    let mut client = TcpStream::connect(address).expect("trickling client connects");
    client
        .set_nodelay(true)
        .expect("individual bytes are sent promptly");
    client
        .set_read_timeout(Some(Duration::from_secs(1)))
        .expect("test response read is bounded");
    client
        .set_write_timeout(Some(Duration::from_secs(1)))
        .expect("test request writes are bounded");
    client.write_all(prefix).expect("partial request writes");

    let (completed, progress_bytes, response) = thread::scope(|scope| {
        let (done_tx, done_rx) = mpsc::channel();
        let server = &server;
        let worker = scope.spawn(move || {
            let result = server.poll_once();
            done_tx.send(()).expect("client waits for the poll result");
            result
        });
        clock.wait_for_reads(1);
        let watchdog = Instant::now() + Duration::from_millis(500);
        let mut progress_bytes = 0;
        let completed = loop {
            let Some(remaining) = watchdog.checked_duration_since(Instant::now()) else {
                break false;
            };
            // This is traffic pacing, not a startup sleep: completion wakes us
            // immediately, and 10 ms progress is below the 100 ms idle timeout.
            match done_rx.recv_timeout(remaining.min(Duration::from_millis(10))) {
                Ok(()) => break true,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    panic!("server poll exited without its completion signal");
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if client.write_all(b"x").is_ok() {
                        progress_bytes += 1;
                    }
                }
            }
        };
        // Also releases the old unbounded implementation on regression failure,
        // so joining its poll thread cannot strand the test process.
        let _ = client.shutdown(std::net::Shutdown::Write);
        let mut response = String::new();
        let read_result = client.read_to_string(&mut response);
        assert!(
            worker
                .join()
                .expect("poll thread exits")
                .expect("peer errors stay local")
        );
        if let Err(error) = read_result {
            // A byte crossing the close can reset TCP after the 408 was sent.
            assert_eq!(error.kind(), std::io::ErrorKind::ConnectionReset);
        }
        (completed, progress_bytes, response)
    });
    assert!(
        completed,
        "progressing request exceeded the cumulative deadline watchdog"
    );
    assert!(
        progress_bytes >= 3,
        "the request must make repeated partial progress"
    );
    assert!(response.starts_with("HTTP/1.1 408"), "{response}");
    assert_eq!(
        app.snapshot()
            .expect("capture remains active")
            .capture_lease_revision,
        Some(lease),
        "the unauthenticated request cannot mutate capture state"
    );

    let revision = app.snapshot().expect("state remains available").revision;
    let body = format!("{{\"action\":\"abort\",\"revision\":{revision}}}");
    let aborted = request_through_poll(
        &server,
        address.port(),
        &action_request(address.port(), server.token(), &body),
    );
    assert!(aborted.starts_with("HTTP/1.1 200"), "{aborted}");
    let snapshot = app.snapshot().expect("abort state remains available");
    assert_eq!(snapshot.phase, "aborted");
    assert_eq!(snapshot.capture_lease_revision, None);
}

fn request(port: u16, request: &str) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("test connects to loopback");
    stream
        .write_all(request.as_bytes())
        .expect("request writes");
    stream
        .shutdown(std::net::Shutdown::Write)
        .expect("request ends");
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .expect("response reads");
    response
}

fn request_through_poll(server: &CalibrationHttpServer, port: u16, request: &str) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("test connects to loopback");
    stream
        .write_all(request.as_bytes())
        .expect("request writes");
    stream
        .shutdown(std::net::Shutdown::Write)
        .expect("request ends");
    assert!(
        server
            .poll_once()
            .expect("server accepts the queued request")
    );
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .expect("response reads");
    response
}

fn action_request(port: u16, token: &str, body: &str) -> String {
    format!(
        "POST /api/action HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nOrigin: http://127.0.0.1:{port}\r\nX-Calibration-Token: {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
}

#[derive(Clone)]
struct DeterministicClock {
    state: Arc<(Mutex<DeterministicClockState>, Condvar)>,
}

struct DeterministicClockState {
    now_ns: u64,
    reads: u64,
}

impl DeterministicClock {
    fn new(now_ns: u64) -> Self {
        Self {
            state: Arc::new((
                Mutex::new(DeterministicClockState { now_ns, reads: 0 }),
                Condvar::new(),
            )),
        }
    }

    fn set(&self, now_ns: u64) {
        self.state.0.lock().expect("clock lock is available").now_ns = now_ns;
    }

    fn wait_for_reads(&self, expected_reads: u64) {
        let (lock, changed) = &*self.state;
        let mut state = lock.lock().expect("clock lock is available");
        while state.reads < expected_reads {
            let (next_state, timeout) = changed
                .wait_timeout(state, Duration::from_secs(2))
                .expect("clock wait lock is available");
            assert!(!timeout.timed_out(), "server did not sample its host clock");
            state = next_state;
        }
    }
}

impl MonotonicHostClock for DeterministicClock {
    fn now_ns(&self) -> u64 {
        let (lock, changed) = &*self.state;
        let mut state = lock.lock().expect("clock lock is available");
        let now_ns = state.now_ns;
        state.reads += 1;
        changed.notify_all();
        now_ns
    }
}
