use invisible_keyboard::{
    calibration::{
        BlockId, CalibrationCoordinator, CalibrationDomain, CalibrationFailure, CalibrationPhase,
        CoordinatorError, KeyboardLayout, KeyboardPrecheckMetrics, LockedArtifactIdentity,
        RejectedExampleReason,
    },
    layout::{PhysicalPosition, all_slots, keyboard_warm_start_eligible},
    validation::{
        PredictionClass, TruthClass, ValidationMatrix, ValidationReport, ValidationReportInput,
    },
};

fn hash(byte: u8) -> [u8; 32] {
    [byte; 32]
}

fn locked_identity(fingerprint: [u8; 32]) -> LockedArtifactIdentity {
    LockedArtifactIdentity::new(fingerprint, hash(0xb5), vec![hash(0xc5)])
        .expect("locked identity has one source session")
}

fn passing_surface_validation(fingerprint: [u8; 32]) -> ValidationReport {
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
        artifact_sha256: fingerprint,
        acquisition_manifest_sha256: hash(0xb5),
        evaluator_version: "surface-evaluator-v1".to_owned(),
        session_sha256: vec![hash(0xd5)],
        evidence_sha256: vec![hash(0xd6)],
        block_ids: vec![40],
        confusion,
        ordinary_motion_exposure_ms: 60_000,
        key_state_neutral: true,
    })
    .expect("passing report is structurally valid")
}

fn enter_keyboard_collection(coordinator: &mut CalibrationCoordinator) {
    coordinator.connect().expect("connect transition is valid");
    coordinator
        .accept_hardware_check()
        .expect("healthy hardware advances");
    coordinator
        .identify_keyboard(KeyboardLayout::UsQwerty)
        .expect("US QWERTY is supported");
    coordinator
        .accept_neutral_pose()
        .expect("stable neutral pose advances");
    assert_eq!(coordinator.phase(), CalibrationPhase::KeyboardCollect);
}

fn keyboard_positions() -> Vec<PhysicalPosition> {
    all_slots()
        .iter()
        .filter(|slot| keyboard_warm_start_eligible(slot.position))
        .map(|slot| slot.position)
        .collect()
}

fn fill_keyboard_coverage(coordinator: &mut CalibrationCoordinator) {
    for position in keyboard_positions() {
        for _ in 0..4 {
            coordinator
                .accept_example(CalibrationDomain::Keyboard, position, BlockId::new(10))
                .expect("first keyboard block is accepted");
        }
        for _ in 0..4 {
            coordinator
                .accept_example(CalibrationDomain::Keyboard, position, BlockId::new(11))
                .expect("second keyboard block is accepted");
        }
    }
}

fn enter_surface_collection(coordinator: &mut CalibrationCoordinator) {
    fill_keyboard_coverage(coordinator);
    coordinator
        .begin_keyboard_precheck(BlockId::new(20))
        .expect("complete keyboard coverage can be validated");
    coordinator
        .submit_keyboard_precheck(
            KeyboardPrecheckMetrics::new(9_500, 9_000).expect("valid metrics"),
            &[],
        )
        .expect("passing keyboard validation advances");
    assert_eq!(coordinator.phase(), CalibrationPhase::SurfaceCollect);
}

fn fill_surface_coverage(coordinator: &mut CalibrationCoordinator) {
    for slot in all_slots() {
        for _ in 0..3 {
            coordinator
                .accept_example(
                    CalibrationDomain::PassiveSurface,
                    slot.position,
                    BlockId::new(30),
                )
                .expect("surface example is accepted");
        }
    }
}

#[test]
fn deterministic_prompts_cover_keyboard_then_all_surface_positions() {
    let mut coordinator = CalibrationCoordinator::new();
    enter_keyboard_collection(&mut coordinator);

    assert_eq!(
        coordinator.next_keyboard_position(),
        keyboard_positions().first().copied()
    );
    assert_eq!(coordinator.progress().keyboard_required, 29 * 8);
    fill_keyboard_coverage(&mut coordinator);
    assert_eq!(coordinator.next_keyboard_position(), None);

    coordinator
        .begin_keyboard_precheck(BlockId::new(20))
        .expect("complete keyboard coverage advances");
    coordinator
        .submit_keyboard_precheck(
            KeyboardPrecheckMetrics::new(9_000, 8_000).expect("valid metrics"),
            &[],
        )
        .expect("threshold equality passes");

    assert_eq!(
        coordinator.next_surface_position(),
        Some(all_slots()[0].position)
    );
    assert_eq!(coordinator.progress().surface_required, 34 * 3);
    fill_surface_coverage(&mut coordinator);
    assert_eq!(coordinator.next_surface_position(), None);
}

#[test]
fn rejected_examples_never_advance_coverage() {
    let mut coordinator = CalibrationCoordinator::new();
    enter_keyboard_collection(&mut coordinator);
    let first = coordinator
        .next_keyboard_position()
        .expect("keyboard has an initial prompt");
    let before = coordinator.progress();

    coordinator
        .reject_example(RejectedExampleReason::AcquisitionFault)
        .expect("rejection is recorded");

    assert_eq!(
        coordinator.progress().keyboard_accepted,
        before.keyboard_accepted
    );
    assert_eq!(coordinator.next_keyboard_position(), Some(first));
    assert_eq!(coordinator.progress().rejected_examples, 1);
}

#[test]
fn training_and_validation_blocks_cannot_overlap() {
    let mut coordinator = CalibrationCoordinator::new();
    enter_keyboard_collection(&mut coordinator);
    let first = coordinator
        .next_keyboard_position()
        .expect("keyboard has an initial prompt");
    coordinator
        .accept_example(CalibrationDomain::Keyboard, first, BlockId::new(7))
        .expect("training block is accepted");
    fill_keyboard_coverage(&mut coordinator);

    assert_eq!(
        coordinator.begin_keyboard_precheck(BlockId::new(7)),
        Err(CoordinatorError::BlockOverlap(BlockId::new(7)))
    );
    assert_eq!(coordinator.phase(), CalibrationPhase::KeyboardCollect);
}

#[test]
fn keyboard_validation_targets_weak_positions_and_stops_on_plateau() {
    let mut coordinator = CalibrationCoordinator::new();
    enter_keyboard_collection(&mut coordinator);
    fill_keyboard_coverage(&mut coordinator);
    let weak = keyboard_positions()[5];

    for (attempt, score) in [8_000, 8_050, 8_100, 8_150].into_iter().enumerate() {
        coordinator
            .begin_keyboard_precheck(BlockId::new(100 + attempt as u32))
            .expect("covered keyboard can be revalidated");
        coordinator
            .submit_keyboard_precheck(
                KeyboardPrecheckMetrics::new(score, 7_500).expect("valid metrics"),
                &[weak],
            )
            .expect("failed validation is a product outcome, not an API error");
        if attempt < 3 {
            assert_eq!(coordinator.phase(), CalibrationPhase::KeyboardCollect);
            coordinator
                .accept_example(
                    CalibrationDomain::Keyboard,
                    weak,
                    BlockId::new(200 + attempt as u32),
                )
                .expect("adaptive weak-key example is accepted");
        }
    }

    assert_eq!(coordinator.phase(), CalibrationPhase::CalibrationFailed);
    assert_eq!(
        coordinator.failure(),
        Some(CalibrationFailure::ValidationPlateau)
    );
}

#[test]
fn duplicate_weak_position_hints_raise_the_target_only_once() {
    let mut coordinator = CalibrationCoordinator::new();
    enter_keyboard_collection(&mut coordinator);
    fill_keyboard_coverage(&mut coordinator);
    let weak = keyboard_positions()[3];
    let before = coordinator.progress().keyboard_required;
    coordinator
        .begin_keyboard_precheck(BlockId::new(50))
        .expect("complete coverage advances");

    coordinator
        .submit_keyboard_precheck(
            KeyboardPrecheckMetrics::new(8_000, 7_000).expect("valid metrics"),
            &[weak, weak],
        )
        .expect("duplicate hints are normalized");

    assert_eq!(coordinator.progress().keyboard_required, before + 1);
}

#[test]
fn a_validation_block_cannot_be_scored_twice_in_different_stages() {
    let mut coordinator = CalibrationCoordinator::new();
    enter_keyboard_collection(&mut coordinator);
    enter_surface_collection(&mut coordinator);
    fill_surface_coverage(&mut coordinator);

    assert_eq!(
        coordinator.begin_artifact_fit(BlockId::new(20)),
        Err(CoordinatorError::DuplicateValidationBlock(BlockId::new(20)))
    );
    assert_eq!(coordinator.phase(), CalibrationPhase::SurfaceCollect);
}

#[test]
fn locked_surface_validation_cannot_mutate_training_state() {
    let mut coordinator = CalibrationCoordinator::new();
    enter_keyboard_collection(&mut coordinator);
    enter_surface_collection(&mut coordinator);
    fill_surface_coverage(&mut coordinator);
    coordinator
        .begin_artifact_fit(BlockId::new(40))
        .expect("surface coverage can be frozen");
    let fingerprint = [0x5a; 32];
    coordinator
        .lock_artifact(locked_identity(fingerprint))
        .expect("artifact locks exactly once");
    let before = coordinator.progress();

    assert_eq!(
        coordinator.accept_example(
            CalibrationDomain::PassiveSurface,
            all_slots()[0].position,
            BlockId::new(41),
        ),
        Err(CoordinatorError::InvalidTransition {
            phase: CalibrationPhase::SurfaceValidate,
            action: "accept example",
        })
    );
    assert_eq!(coordinator.progress(), before);
    assert_eq!(coordinator.artifact_fingerprint(), Some(&fingerprint));
}

#[test]
fn complete_locked_validation_reaches_validation_passed() {
    let mut coordinator = CalibrationCoordinator::new();
    enter_keyboard_collection(&mut coordinator);
    enter_surface_collection(&mut coordinator);
    fill_surface_coverage(&mut coordinator);
    coordinator
        .begin_artifact_fit(BlockId::new(40))
        .expect("surface collection freezes");
    coordinator
        .lock_artifact(locked_identity([0xa5; 32]))
        .expect("artifact locks");

    let validation = passing_surface_validation([0xa5; 32]);
    coordinator
        .submit_surface_validation(&validation)
        .expect("passing validation advances");

    assert_eq!(coordinator.phase(), CalibrationPhase::ValidationPassed);
    assert_eq!(coordinator.failure(), None);

    coordinator.report_hardware_fault();
    assert_eq!(
        coordinator.phase(),
        CalibrationPhase::NeedsHardwareAttention
    );
    assert_eq!(
        coordinator.failure(),
        Some(CalibrationFailure::HardwareFault)
    );
}

#[test]
fn abort_revokes_a_validation_passed_state() {
    let mut coordinator = CalibrationCoordinator::new();
    enter_keyboard_collection(&mut coordinator);
    enter_surface_collection(&mut coordinator);
    fill_surface_coverage(&mut coordinator);
    coordinator
        .begin_artifact_fit(BlockId::new(40))
        .expect("surface collection freezes");
    coordinator
        .lock_artifact(locked_identity([0xa5; 32]))
        .expect("artifact locks");
    let validation = passing_surface_validation([0xa5; 32]);
    coordinator
        .submit_surface_validation(&validation)
        .expect("validation passes");

    coordinator.abort();

    assert_eq!(coordinator.phase(), CalibrationPhase::Aborted);
    assert_eq!(coordinator.failure(), None);
}

#[test]
fn elapsed_time_cap_terminates_instead_of_requesting_infinite_text() {
    let mut coordinator = CalibrationCoordinator::new();
    enter_keyboard_collection(&mut coordinator);

    coordinator
        .observe_elapsed_seconds(1_200)
        .expect("monotonic elapsed time is accepted");

    assert_eq!(coordinator.phase(), CalibrationPhase::CalibrationFailed);
    assert_eq!(coordinator.failure(), Some(CalibrationFailure::TimeCap));
}
