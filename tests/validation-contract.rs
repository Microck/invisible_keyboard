use invisible_keyboard::{
    calibration::{BlockId, LockedArtifactIdentity},
    layout::{KeyUsage, PHYSICAL_POSITION_COUNT, all_slots},
    validation::{
        NULL_PREDICTION_COLUMN, PREDICTION_CLASS_COUNT, PredictionClass, TRUTH_CLASS_COUNT,
        TruthClass, ValidationEvidenceError, ValidationMatrix, ValidationReport,
        ValidationReportError, ValidationReportInput,
    },
};

fn hash(byte: u8) -> [u8; 32] {
    [byte; 32]
}

fn identity() -> LockedArtifactIdentity {
    LockedArtifactIdentity::new(hash(1), hash(2), vec![hash(3), hash(4)])
        .expect("identity is complete")
}

fn perfect_matrix() -> ValidationMatrix {
    let mut matrix = ValidationMatrix::new();
    for slot in all_slots() {
        for _ in 0..3 {
            let prediction = slot
                .usage
                .map_or(PredictionClass::Null, PredictionClass::Key);
            matrix.observe(TruthClass::Position(slot.position), prediction);
        }
    }
    for _ in 0..20 {
        matrix.observe(TruthClass::Null, PredictionClass::Null);
    }
    matrix
}

fn report(matrix: ValidationMatrix) -> ValidationReport {
    ValidationReport::new(ValidationReportInput {
        artifact_sha256: hash(1),
        acquisition_manifest_sha256: hash(2),
        evaluator_version: "surface-evaluator-v1".to_owned(),
        session_sha256: vec![hash(8)],
        evidence_sha256: vec![hash(9)],
        block_ids: vec![40],
        confusion: matrix,
        ordinary_motion_exposure_ms: 60_000,
        key_state_neutral: true,
    })
    .expect("report structure is valid")
}

#[test]
fn perfect_count_level_report_derives_metrics_and_passes() {
    let report = report(perfect_matrix());
    let metrics = report
        .validate_against(&identity(), BlockId::new(40), &[])
        .expect("held-out evidence binds");

    assert_eq!(metrics.macro_f1_basis_points, 10_000);
    assert_eq!(metrics.minimum_recall_basis_points, 10_000);
    assert_eq!(report.reserved_activations(), 0);
    assert_eq!(report.ordinary_motion_activations(), 0);
    assert!(report.passes_held_out_validation(metrics));

    let mut short_exposure = report.clone();
    short_exposure.ordinary_motion_exposure_ms = 59_999;
    assert!(!short_exposure.passes_held_out_validation(metrics));
}

#[test]
fn every_physical_position_and_null_truth_need_evidence() {
    let mut matrix = perfect_matrix();
    matrix.clear_truth_row(5);
    let incomplete = report(matrix);
    assert_eq!(
        incomplete.validate_against(&identity(), BlockId::new(40), &[]),
        Err(ValidationEvidenceError::MissingPositionEvidence {
            position: all_slots()[5].position,
            actual: 0,
        })
    );

    let mut matrix = perfect_matrix();
    matrix.clear_truth_row(PHYSICAL_POSITION_COUNT);
    let no_null = report(matrix);
    assert_eq!(
        no_null.validate_against(&identity(), BlockId::new(40), &[]),
        Err(ValidationEvidenceError::MissingNullEvidence)
    );
}

#[test]
fn artifact_manifest_session_and_block_binding_fail_closed() {
    let mut wrong_artifact = report(perfect_matrix());
    wrong_artifact.artifact_sha256 =
        "0909090909090909090909090909090909090909090909090909090909090909".to_owned();
    assert_eq!(
        wrong_artifact.validate_against(&identity(), BlockId::new(40), &[]),
        Err(ValidationEvidenceError::ArtifactMismatch)
    );

    let mut wrong_manifest = report(perfect_matrix());
    wrong_manifest.acquisition_manifest_sha256 =
        "0909090909090909090909090909090909090909090909090909090909090909".to_owned();
    assert_eq!(
        wrong_manifest.validate_against(&identity(), BlockId::new(40), &[]),
        Err(ValidationEvidenceError::ManifestMismatch)
    );

    let mut training_session = report(perfect_matrix());
    training_session.session_sha256[0] =
        "0303030303030303030303030303030303030303030303030303030303030303".to_owned();
    assert_eq!(
        training_session.validate_against(&identity(), BlockId::new(40), &[]),
        Err(ValidationEvidenceError::TrainingSessionOverlap)
    );

    let wrong_block = report(perfect_matrix());
    assert_eq!(
        wrong_block.validate_against(&identity(), BlockId::new(41), &[]),
        Err(ValidationEvidenceError::MissingValidationBlock)
    );
    assert_eq!(
        wrong_block.validate_against(&identity(), BlockId::new(40), &[40]),
        Err(ValidationEvidenceError::TrainingBlockOverlap(40))
    );
}

#[test]
fn reserved_and_null_rows_expose_false_activations() {
    let mut matrix = perfect_matrix();
    let reserved = all_slots()
        .iter()
        .find(|slot| slot.usage.is_none())
        .expect("reserved position exists")
        .position;
    matrix.observe(
        TruthClass::Position(reserved),
        PredictionClass::Key(KeyUsage::A),
    );
    matrix.observe(TruthClass::Null, PredictionClass::Key(KeyUsage::B));
    let report = report(matrix);
    let metrics = report
        .validate_against(&identity(), BlockId::new(40), &[])
        .expect("report remains structurally valid");

    assert_eq!(report.reserved_activations(), 1);
    assert_eq!(report.ordinary_motion_activations(), 1);
    assert!(!report.passes_held_out_validation(metrics));
}

#[test]
fn matrix_shape_and_report_json_are_strict_and_stable() {
    let report = report(perfect_matrix());
    assert_eq!(
        report.confusion.cells().len(),
        TRUTH_CLASS_COUNT * PREDICTION_CLASS_COUNT
    );
    assert_eq!(report.confusion.truth_total(PHYSICAL_POSITION_COUNT), 20);
    assert_eq!(
        report
            .confusion
            .count(PHYSICAL_POSITION_COUNT, NULL_PREDICTION_COLUMN),
        20
    );

    let bytes = report.canonical_json().expect("report serializes");
    let recovered = ValidationReport::from_json(&bytes).expect("report parses");
    assert_eq!(
        bytes,
        recovered.canonical_json().expect("report reserializes")
    );
    assert_eq!(
        report.fingerprint().expect("report hashes"),
        recovered.fingerprint().expect("recovered report hashes")
    );

    let mut unknown = String::from_utf8(bytes).expect("report JSON is UTF-8");
    unknown.insert_str(unknown.len() - 1, ",\"claimed_accuracy\":10000");
    assert!(ValidationReport::from_json(unknown.as_bytes()).is_err());
}

#[test]
fn every_validation_session_has_one_bound_evidence_sidecar() {
    let mut missing = report(perfect_matrix());
    missing.evidence_sha256.clear();
    assert_eq!(
        missing.validate_structure(),
        Err(ValidationReportError::EvidenceSessionCountMismatch)
    );

    let mut repeated = report(perfect_matrix());
    repeated.session_sha256.push("0a".repeat(32));
    repeated
        .evidence_sha256
        .push(repeated.evidence_sha256[0].clone());
    assert_eq!(
        repeated.validate_structure(),
        Err(ValidationReportError::DuplicateEvidence)
    );
}
