use std::collections::BTreeMap;

use invisible_keyboard::{
    layout::{Hand, all_slots},
    manifest::{format_sha256, sha256},
    physical_study::{
        CandidateObservation, CriterionOutcome, FrozenStudyPipeline, GateDisposition,
        IndependentTruthSource, ObservationRepresentation, ObservationSource,
        PHYSICAL_STUDY_ANALYSIS_PLAN_MAX_BYTES, PHYSICAL_STUDY_EVIDENCE_JSONL_MAX_BYTES,
        PHYSICAL_STUDY_EVIDENCE_ROW_MAX_BYTES, PHYSICAL_STUDY_EVIDENCE_SCHEMA,
        PHYSICAL_STUDY_REPORT_MAX_BYTES, PHYSICAL_STUDY_REPORT_SCHEMA,
        PHYSICAL_STUDY_SPLIT_MANIFEST_MAX_BYTES, PhysicalStudyAnalysisPlan, PhysicalStudyArtifact,
        PhysicalStudyError, PhysicalStudyEvidenceRow, PhysicalStudyGate, PhysicalStudyReport,
        PhysicalStudySplitManifest, PromptTruthClock, StudyCondition, StudySplit,
        analyze_physical_study_jsonl, decode_physical_study_analysis_plan,
        decode_physical_study_evidence_row, decode_physical_study_jsonl,
        decode_physical_study_report, decode_physical_study_split_manifest,
        encode_physical_study_jsonl, seal_physical_study_evidence_row,
        validate_physical_study_artifact_size,
    },
};

const FINGERS: [&str; 5] = ["thumb", "index", "middle", "ring", "pinky"];

struct Fixture {
    rows: Vec<PhysicalStudyEvidenceRow>,
    next_ordinal: BTreeMap<String, u64>,
    tap_index: usize,
    contact_detector_sha256: String,
    transform_sha256: String,
}

struct FixtureRow<'a> {
    gate: PhysicalStudyGate,
    split: StudySplit,
    participant: &'a str,
    mount: &'a str,
    session: &'a str,
    hand: Hand,
    condition: StudyCondition,
    label: Option<&'a str>,
    features: Vec<i32>,
    miss_every: Option<usize>,
}

impl Fixture {
    fn new() -> Self {
        Self {
            rows: Vec::new(),
            next_ordinal: BTreeMap::new(),
            tap_index: 0,
            contact_detector_sha256: hash("contact-detector"),
            transform_sha256: hash("fixed-feature-transform"),
        }
    }

    fn push(&mut self, spec: FixtureRow<'_>) {
        let FixtureRow {
            gate,
            split,
            participant,
            mount,
            session,
            hand,
            condition,
            label,
            features,
            miss_every,
        } = spec;
        let ordinal = self.next_ordinal.entry(session.to_owned()).or_default();
        let current_ordinal = *ordinal;
        *ordinal += 1;
        // A full minute of each frozen negative condition prevents a token
        // interval from masquerading as meaningful false-activation exposure.
        let base = current_ordinal * 61_000_000;
        let intentional = condition == StudyCondition::IntentionalTap;
        let truth_event_time_us = intentional.then_some(base + 200_000);
        let should_miss =
            intentional && miss_every.is_some_and(|stride| self.tap_index.is_multiple_of(stride));
        if intentional {
            self.tap_index += 1;
        }
        let prompt_id = format!("{session}-prompt-{current_ordinal:06}");
        let candidate_events = if intentional && !should_miss {
            vec![CandidateObservation {
                event_id: format!("{prompt_id}-event"),
                event_time_us: truth_event_time_us.expect("intentional row has truth") + 10_000,
                features,
            }]
        } else {
            Vec::new()
        };
        let row = PhysicalStudyEvidenceRow {
            schema: PHYSICAL_STUDY_EVIDENCE_SCHEMA.to_owned(),
            row_sha256: String::new(),
            gate,
            round_id: "frozen-round-1".to_owned(),
            split,
            split_manifest_sha256: String::new(),
            analysis_plan_sha256: String::new(),
            contact_detector_sha256: self.contact_detector_sha256.clone(),
            contact_detector_fit_splits: vec![StudySplit::Train, StudySplit::Validation],
            participant_id: participant.to_owned(),
            session_id: session.to_owned(),
            rig_id: format!("rig-{participant}"),
            controller_id: format!("controller-{participant}-{hand:?}"),
            mount_id: mount.to_owned(),
            surface_id: "rigid-desk-1".to_owned(),
            hand,
            condition,
            prompted_label: label.map(str::to_owned),
            truth_label: label.map(str::to_owned),
            truth_source: IndependentTruthSource::HighSpeedVideo,
            clock: PromptTruthClock {
                prompt_id,
                prompt_ordinal: current_ordinal,
                timeline_id: format!("timeline-{session}"),
                clock_mapping_sha256: hash(&format!("clock-{session}")),
                prompt_time_us: base,
                truth_start_us: base + 100_000,
                truth_event_time_us,
                truth_end_us: if intentional {
                    base + 500_000
                } else {
                    base + 60_100_000
                },
            },
            observation_source: ObservationSource {
                representation: ObservationRepresentation::Derived,
                artifact_sha256: hash(&format!("artifact-{split:?}-{session}")),
                transform_sha256: Some(self.transform_sha256.clone()),
                transform_fit_splits: vec![StudySplit::Train],
            },
            candidate_events,
        };
        self.rows.push(row);
    }

    fn push_negative_conditions(
        &mut self,
        gate: PhysicalStudyGate,
        participant: &str,
        mount: &str,
        session: &str,
        hand: Hand,
    ) {
        let conditions: &[StudyCondition] = match gate {
            PhysicalStudyGate::Gate0B => &[StudyCondition::Quiet, StudyCondition::ScriptedMotion],
            PhysicalStudyGate::Gate1 => &[
                StudyCondition::Quiet,
                StudyCondition::SharedMotion,
                StudyCondition::PhysicalKeyboardTyping,
                StudyCondition::DeskTapping,
                StudyCondition::SimultaneousAmbiguous,
            ],
            PhysicalStudyGate::Gate2 => &[],
        };
        for &condition in conditions {
            self.push(FixtureRow {
                gate,
                split: StudySplit::Heldout,
                participant,
                mount,
                session,
                hand,
                condition,
                label: None,
                features: Vec::new(),
                miss_every: None,
            });
        }
    }

    fn artifacts(&mut self) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let manifest = PhysicalStudySplitManifest::from_rows(&self.rows)
            .expect("fixture split manifest freezes");
        let manifest_bytes = manifest
            .canonical_json_bytes()
            .expect("fixture manifest serializes");
        let manifest_hash = hash_bytes(&manifest_bytes);
        let plan = PhysicalStudyAnalysisPlan::v1(
            manifest.gate,
            manifest.round_id.clone(),
            manifest_hash.clone(),
            FrozenStudyPipeline::from(&self.rows[0]),
        );
        let plan_bytes = plan
            .canonical_json_bytes()
            .expect("fixture analysis plan serializes");
        let plan_hash = hash_bytes(&plan_bytes);
        for row in &mut self.rows {
            row.split_manifest_sha256.clone_from(&manifest_hash);
            row.analysis_plan_sha256.clone_from(&plan_hash);
            seal_physical_study_evidence_row(row).expect("frozen fixture row seals");
        }
        (
            encode_physical_study_jsonl(&self.rows).expect("fixture JSONL encodes"),
            manifest_bytes,
            plan_bytes,
        )
    }
}

fn analyze_fixture(fixture: &mut Fixture) -> PhysicalStudyReport {
    let (evidence, manifest, plan) = fixture.artifacts();
    analyze_physical_study_jsonl(&evidence, &manifest, &plan).expect("fixture scores")
}

fn hash(value: &str) -> String {
    hash_bytes(value.as_bytes())
}

fn one_hot(index: usize, dimensions: usize) -> Vec<i32> {
    let mut values = vec![0; dimensions];
    values[index] = 10_000;
    values
}

fn gate0b_fixture(miss_every: Option<usize>) -> Fixture {
    let mut fixture = Fixture::new();
    for participant_index in 0..5 {
        let participant = format!("participant-{participant_index}");
        for mount_index in 0..2 {
            let mount = format!("{participant}-mount-{mount_index}");
            for hand in [Hand::Left, Hand::Right] {
                let session = format!("{participant}-session-{mount_index}-{hand:?}");
                for finger in FINGERS {
                    for _ in 0..50 {
                        fixture.push(FixtureRow {
                            gate: PhysicalStudyGate::Gate0B,
                            split: StudySplit::Heldout,
                            participant: &participant,
                            mount: &mount,
                            session: &session,
                            hand,
                            condition: StudyCondition::IntentionalTap,
                            label: Some(finger),
                            features: Vec::new(),
                            miss_every,
                        });
                    }
                }
                fixture.push_negative_conditions(
                    PhysicalStudyGate::Gate0B,
                    &participant,
                    &mount,
                    &session,
                    hand,
                );
            }
        }
    }
    fixture
}

fn gate1_fixture() -> Fixture {
    let mut fixture = Fixture::new();
    for (split, participant) in [
        (StudySplit::Train, "training-person"),
        (StudySplit::Validation, "validation-person"),
    ] {
        let mount = format!("{participant}-mount");
        let session = format!("{participant}-session");
        for (index, finger) in FINGERS.into_iter().enumerate() {
            fixture.push(FixtureRow {
                gate: PhysicalStudyGate::Gate1,
                split,
                participant,
                mount: &mount,
                session: &session,
                hand: Hand::Left,
                condition: StudyCondition::IntentionalTap,
                label: Some(finger),
                features: one_hot(index, FINGERS.len()),
                miss_every: None,
            });
        }
    }
    for participant_index in 0..5 {
        let participant = format!("participant-{participant_index}");
        for mount_index in 0..2 {
            let mount = format!("{participant}-mount-{mount_index}");
            for hand in [Hand::Left, Hand::Right] {
                let session = format!("{participant}-session-{mount_index}-{hand:?}");
                for (index, finger) in FINGERS.into_iter().enumerate() {
                    for _ in 0..50 {
                        fixture.push(FixtureRow {
                            gate: PhysicalStudyGate::Gate1,
                            split: StudySplit::Heldout,
                            participant: &participant,
                            mount: &mount,
                            session: &session,
                            hand,
                            condition: StudyCondition::IntentionalTap,
                            label: Some(finger),
                            features: one_hot(index, FINGERS.len()),
                            miss_every: None,
                        });
                    }
                }
                fixture.push_negative_conditions(
                    PhysicalStudyGate::Gate1,
                    &participant,
                    &mount,
                    &session,
                    hand,
                );
            }
        }
    }
    fixture
}

fn gate2_fixture() -> Fixture {
    let mut fixture = Fixture::new();
    let slots = all_slots();
    for (split, participant) in [
        (StudySplit::Train, "training-person"),
        (StudySplit::Validation, "validation-person"),
    ] {
        let mount = format!("{participant}-mount");
        let session = format!("{participant}-session");
        for (index, slot) in slots.iter().enumerate() {
            fixture.push(FixtureRow {
                gate: PhysicalStudyGate::Gate2,
                split,
                participant,
                mount: &mount,
                session: &session,
                hand: slot.position.hand,
                condition: StudyCondition::IntentionalTap,
                label: Some(slot.label),
                features: one_hot(index, slots.len()),
                miss_every: None,
            });
        }
    }
    for participant_index in 0..8 {
        let participant = format!("participant-{participant_index}");
        for mount_index in 0..2 {
            let mount = format!("{participant}-mount-{mount_index}");
            let session = format!("{participant}-session-{mount_index}");
            for (index, slot) in slots.iter().enumerate() {
                for _ in 0..50 {
                    fixture.push(FixtureRow {
                        gate: PhysicalStudyGate::Gate2,
                        split: StudySplit::Heldout,
                        participant: &participant,
                        mount: &mount,
                        session: &session,
                        hand: slot.position.hand,
                        condition: StudyCondition::IntentionalTap,
                        label: Some(slot.label),
                        features: one_hot(index, slots.len()),
                        miss_every: None,
                    });
                }
            }
        }
    }
    fixture
}

#[test]
fn gate0b_scores_contact_timing_and_preserves_inconclusive() {
    let mut perfect = gate0b_fixture(None);
    let (perfect_source, manifest, plan) = perfect.artifacts();
    let first = analyze_physical_study_jsonl(&perfect_source, &manifest, &plan)
        .expect("perfect contact fixture");
    let second =
        analyze_physical_study_jsonl(&perfect_source, &manifest, &plan).expect("repeat score");

    assert_eq!(first, second);
    assert_eq!(first.schema, PHYSICAL_STUDY_REPORT_SCHEMA);
    assert_eq!(first.disposition, GateDisposition::Go);
    assert_eq!(first.cohort.participant_count, 5);
    assert_eq!(first.cohort.mount_count, 10);
    assert_eq!(first.metrics.positive_trials, 5_000);
    assert_eq!(first.metrics.misses, 0);
    assert!(first.metrics.miss_upper_95_ppm.unwrap() > 0);
    assert_eq!(first.metrics.latency_p95_us, Some(10_000));
    assert_eq!(first.metrics.latency_p95_upper_95_us, Some(10_000));
    assert_eq!(first.metrics.latency_p99_us, Some(10_000));
    assert!(
        first
            .metrics
            .participant_minimum_class_recall_lower_95_ppm
            .unwrap()
            < 1_000_000
    );
    assert_eq!(first.metrics.false_activations, 0);
    assert_eq!(first.source.exact_jsonl_sha256, hash_bytes(&perfect_source));
    assert!(first.frozen_model.is_none());

    let mut borderline_fixture = gate0b_fixture(None);
    for (index, row) in borderline_fixture
        .rows
        .iter_mut()
        .filter(|row| {
            row.participant_id == "participant-0" && row.condition == StudyCondition::IntentionalTap
        })
        .enumerate()
    {
        if index.is_multiple_of(5) {
            row.candidate_events.clear();
        }
    }
    let inconclusive = analyze_fixture(&mut borderline_fixture);
    assert_eq!(inconclusive.disposition, GateDisposition::Inconclusive);
    assert!(inconclusive.metrics.miss_rate_ppm.unwrap() <= 50_000);
    assert!(inconclusive.metrics.miss_upper_95_ppm.unwrap() > 50_000);

    let mut failed_fixture = gate0b_fixture(Some(16));
    let failed = analyze_fixture(&mut failed_fixture);
    assert_eq!(failed.disposition, GateDisposition::NoGo);
    assert!(failed.metrics.miss_rate_ppm.unwrap() > 50_000);
}

#[test]
fn gate1_fits_only_train_selects_only_validation_and_scores_remounts() {
    let mut fixture = gate1_fixture();
    let report = analyze_fixture(&mut fixture);

    assert_eq!(report.gate, PhysicalStudyGate::Gate1);
    assert_eq!(report.disposition, GateDisposition::Go);
    assert_eq!(report.cohort.participant_count, 5);
    assert_eq!(report.participant_groups.len(), 5);
    assert_eq!(report.remount_groups.len(), 10);
    assert_eq!(report.metrics.participant_macro_f1_ppm, Some(1_000_000));
    assert!(report.metrics.participant_macro_f1_lower_95_ppm.unwrap() >= 900_000);
    assert!(report.metrics.participant_macro_f1_lower_95_ppm.unwrap() < 1_000_000);
    assert!(report.metrics.event_error_upper_95_ppm.unwrap() > 0);
    assert_eq!(report.metrics.per_class.len(), 5);
    assert_eq!(
        report
            .metrics
            .confusion_matrix
            .as_ref()
            .expect("Gate 1 matrix")
            .labels,
        ["thumb", "index", "middle", "ring", "pinky", "null"]
    );
    let model = report.frozen_model.expect("Gate 1 freezes a model");
    assert_eq!(model.train_event_count, 5);
    assert_eq!(model.validation_row_count, 5);
    assert_eq!(model.threshold_squared_distance_bits, 0.0_f64.to_bits());
}

#[test]
fn gate2_enforces_fifty_per_position_and_scores_reserved_abstention() {
    let mut fixture = gate2_fixture();
    let (evidence, manifest, plan) = fixture.artifacts();
    let maximum_row_bytes = evidence
        .split(|byte| *byte == b'\n')
        .map(<[u8]>::len)
        .max()
        .expect("fixture has evidence rows");
    assert!(evidence.len() <= PHYSICAL_STUDY_EVIDENCE_JSONL_MAX_BYTES / 4);
    assert!(maximum_row_bytes <= PHYSICAL_STUDY_EVIDENCE_ROW_MAX_BYTES / 4);
    assert!(manifest.len() <= PHYSICAL_STUDY_SPLIT_MANIFEST_MAX_BYTES / 4);
    assert!(plan.len() <= PHYSICAL_STUDY_ANALYSIS_PLAN_MAX_BYTES / 4);
    let report = analyze_physical_study_jsonl(&evidence, &manifest, &plan).expect("fixture scores");
    let report_bytes = report.canonical_json_bytes().expect("report serializes");
    assert!(report_bytes.len() <= PHYSICAL_STUDY_REPORT_MAX_BYTES / 4);

    assert_eq!(report.gate, PhysicalStudyGate::Gate2);
    assert_eq!(report.disposition, GateDisposition::Go);
    assert_eq!(report.cohort.participant_count, 8);
    assert_eq!(report.cohort.mount_count, 16);
    assert_eq!(report.metrics.positive_trials, 27_200);
    assert_eq!(report.metrics.event_errors, 0);
    assert_eq!(report.metrics.per_class.len(), 34);
    for reserved in [
        "left-outer-thumb",
        "comma",
        "period",
        "semicolon",
        "slash",
        "right-outer-thumb",
    ] {
        let metric = report
            .metrics
            .per_class
            .iter()
            .find(|metric| metric.label == reserved)
            .expect("reserved position is reported");
        assert_eq!(metric.support, 800);
        assert_eq!(metric.recall_ppm, 1_000_000);
    }
    let matrix = report
        .metrics
        .confusion_matrix
        .as_ref()
        .expect("Gate 2 matrix");
    assert_eq!(matrix.labels.len(), 29);
    assert_eq!(matrix.labels.last().map(String::as_str), Some("null"));
    assert_eq!(
        report
            .frozen_model
            .as_ref()
            .expect("Gate 2 model")
            .class_labels
            .len(),
        34
    );
}

#[test]
fn semantic_tamper_is_rejected_before_scoring() {
    let mut fixture = gate0b_fixture(None);
    let (_, manifest, plan) = fixture.artifacts();
    fixture.rows[0].candidate_events[0].event_time_us += 1;
    let evidence = encode_physical_study_jsonl(&fixture.rows).expect("tampered JSONL encodes");

    let error =
        analyze_physical_study_jsonl(&evidence, &manifest, &plan).expect_err("tamper must fail");
    assert!(matches!(error, PhysicalStudyError::RowHashMismatch { .. }));
}

fn push_boundary_tap(
    fixture: &mut Fixture,
    session: &str,
    truth_start_us: u64,
    truth_event_time_us: u64,
    truth_end_us: u64,
    candidate_time_us: u64,
) {
    fixture.push(FixtureRow {
        gate: PhysicalStudyGate::Gate0B,
        split: StudySplit::Heldout,
        participant: "participant-0",
        mount: "participant-0-mount-0",
        session,
        hand: Hand::Left,
        condition: StudyCondition::IntentionalTap,
        label: Some("thumb"),
        features: Vec::new(),
        miss_every: None,
    });
    let row = fixture.rows.last_mut().expect("boundary tap was appended");
    row.clock.prompt_time_us = truth_start_us;
    row.clock.truth_start_us = truth_start_us;
    row.clock.truth_event_time_us = Some(truth_event_time_us);
    row.clock.truth_end_us = truth_end_us;
    row.candidate_events[0].event_time_us = candidate_time_us;
}

#[test]
fn sealed_truth_window_underflow_is_rejected_before_scoring() {
    let mut fixture = gate0b_fixture(None);
    push_boundary_tap(
        &mut fixture,
        "underflow-session",
        0,
        25_000,
        275_000,
        25_000,
    );
    let row_id = fixture.rows.last().unwrap().clock.prompt_id.clone();
    let (evidence, manifest, plan) = fixture.artifacts();

    let error = analyze_physical_study_jsonl(&evidence, &manifest, &plan)
        .expect_err("a sealed row cannot clip the required pre-event truth interval");
    assert!(matches!(
        error,
        PhysicalStudyError::InvalidField { row_id: invalid_row, .. }
            if invalid_row == row_id
    ));
}

#[test]
fn sealed_truth_window_overflow_is_rejected_before_scoring() {
    let mut fixture = gate0b_fixture(None);
    let truth_time = u64::MAX - 249_999;
    push_boundary_tap(
        &mut fixture,
        "overflow-session",
        truth_time - 50_000,
        truth_time,
        u64::MAX,
        truth_time,
    );
    let row_id = fixture.rows.last().unwrap().clock.prompt_id.clone();
    let (evidence, manifest, plan) = fixture.artifacts();

    let error = analyze_physical_study_jsonl(&evidence, &manifest, &plan)
        .expect_err("a sealed row cannot clip the required post-event truth interval");
    assert!(matches!(
        error,
        PhysicalStudyError::InvalidField { row_id: invalid_row, .. }
            if invalid_row == row_id
    ));
}

#[test]
fn complete_truth_windows_match_exact_timestamp_endpoints() {
    let mut fixture = gate0b_fixture(None);
    push_boundary_tap(&mut fixture, "lower-bound-session", 0, 50_000, 300_000, 0);
    push_boundary_tap(
        &mut fixture,
        "upper-bound-session",
        u64::MAX - 300_000,
        u64::MAX - 250_000,
        u64::MAX,
        u64::MAX,
    );

    let report = analyze_fixture(&mut fixture);
    assert_eq!(report.metrics.positive_trials, 5_002);
    assert_eq!(report.metrics.misses, 0);
    assert_eq!(report.metrics.extra_events, 0);
    assert_eq!(report.metrics.latency_min_us, Some(-50_000));
    assert_eq!(report.metrics.matched_events, 5_002);
}

#[test]
fn source_artifact_cannot_cross_a_split() {
    let mut fixture = gate1_fixture();
    let train_artifact = fixture
        .rows
        .iter()
        .find(|row| row.split == StudySplit::Train)
        .expect("train row")
        .observation_source
        .artifact_sha256
        .clone();
    let heldout = fixture
        .rows
        .iter_mut()
        .find(|row| row.split == StudySplit::Heldout)
        .expect("heldout row");
    heldout.observation_source.artifact_sha256 = train_artifact;

    let (evidence, manifest, plan) = fixture.artifacts();
    let error =
        analyze_physical_study_jsonl(&evidence, &manifest, &plan).expect_err("leakage must fail");
    assert!(matches!(error, PhysicalStudyError::Leakage(_)));
}

#[test]
fn participant_identity_cannot_cross_a_split() {
    let mut fixture = gate1_fixture();
    for row in fixture
        .rows
        .iter_mut()
        .filter(|row| row.split == StudySplit::Train)
    {
        row.participant_id = "participant-0".to_owned();
    }
    let (evidence, manifest, plan) = fixture.artifacts();

    let error = analyze_physical_study_jsonl(&evidence, &manifest, &plan)
        .expect_err("participant leakage must fail");
    assert!(matches!(error, PhysicalStudyError::Leakage(_)));
}

#[test]
fn frozen_roster_rejects_optional_stopping_or_row_removal() {
    let mut fixture = gate0b_fixture(None);
    let (_, manifest, plan) = fixture.artifacts();
    fixture.rows.pop().expect("fixture has a final row");
    let shortened = encode_physical_study_jsonl(&fixture.rows).expect("shortened JSONL encodes");

    let error = analyze_physical_study_jsonl(&shortened, &manifest, &plan)
        .expect_err("frozen roster must reject removal");
    assert!(matches!(error, PhysicalStudyError::Leakage(_)));
}

#[test]
fn analysis_plan_contents_cannot_be_retuned_after_collection() {
    let mut fixture = gate1_fixture();
    let (_, manifest, plan) = fixture.artifacts();
    let mut retuned = decode_physical_study_analysis_plan(&plan).expect("frozen plan parses");
    retuned.event_error_upper_limit_ppm += 1;
    let retuned_bytes = retuned
        .canonical_json_bytes()
        .expect("retuned plan encodes");
    let retuned_hash = hash_bytes(&retuned_bytes);
    for row in &mut fixture.rows {
        row.analysis_plan_sha256.clone_from(&retuned_hash);
        seal_physical_study_evidence_row(row).expect("retuned fixture row seals");
    }
    let evidence = encode_physical_study_jsonl(&fixture.rows).expect("retuned JSONL encodes");

    let error = analyze_physical_study_jsonl(&evidence, &manifest, &retuned_bytes)
        .expect_err("post-collection threshold tuning must fail");
    assert!(matches!(error, PhysicalStudyError::Leakage(_)));
}

#[test]
fn both_hands_are_required_in_every_heldout_remount() {
    let mut fixture = gate1_fixture();
    fixture.rows.retain(|row| {
        !(row.participant_id == "participant-0"
            && row.mount_id == "participant-0-mount-0"
            && row.hand == Hand::Right)
    });
    let (evidence, manifest, plan) = fixture.artifacts();

    let error = analyze_physical_study_jsonl(&evidence, &manifest, &plan)
        .expect_err("missing right hand must fail");
    assert!(matches!(error, PhysicalStudyError::MissingStratum(_)));
}

#[test]
fn token_negative_exposure_cannot_satisfy_a_heldout_stratum() {
    let mut fixture = gate1_fixture();
    let quiet = fixture
        .rows
        .iter_mut()
        .find(|row| row.split == StudySplit::Heldout && row.condition == StudyCondition::Quiet)
        .expect("heldout quiet interval");
    quiet.clock.truth_end_us = quiet.clock.truth_start_us + 59_999_999;
    let (evidence, manifest, plan) = fixture.artifacts();

    let error = analyze_physical_study_jsonl(&evidence, &manifest, &plan)
        .expect_err("sub-minute negative exposure must fail");
    assert!(matches!(error, PhysicalStudyError::MissingStratum(_)));
}

#[test]
fn negative_outputs_and_duplicate_contacts_are_decisive_failures() {
    let mut gate1 = gate1_fixture();
    let negative = gate1
        .rows
        .iter_mut()
        .find(|row| row.split == StudySplit::Heldout && row.condition == StudyCondition::Quiet)
        .expect("heldout quiet interval");
    negative.candidate_events.push(CandidateObservation {
        event_id: format!("{}-false", negative.clock.prompt_id),
        event_time_us: negative.clock.truth_start_us + 100_000,
        features: one_hot(0, FINGERS.len()),
    });
    let gate1_report = analyze_fixture(&mut gate1);
    assert_eq!(gate1_report.disposition, GateDisposition::NoGo);
    assert!(gate1_report.metrics.false_activations > 0);
    assert_eq!(
        gate1_report.metrics.matched_events,
        gate1_report.metrics.positive_trials
    );
    assert_eq!(gate1_report.metrics.extra_events, 1);

    let mut gate0b = gate0b_fixture(None);
    let tap = gate0b
        .rows
        .iter_mut()
        .find(|row| row.condition == StudyCondition::IntentionalTap)
        .expect("heldout tap");
    tap.candidate_events.push(CandidateObservation {
        event_id: format!("{}-duplicate", tap.clock.prompt_id),
        event_time_us: tap.clock.truth_event_time_us.expect("tap truth") + 20_000,
        features: Vec::new(),
    });
    let gate0b_report = analyze_fixture(&mut gate0b);
    assert_eq!(gate0b_report.disposition, GateDisposition::NoGo);
    assert_eq!(gate0b_report.metrics.duplicate_events, 1);
    assert_eq!(gate0b_report.metrics.extra_events, 1);
}

#[test]
fn anticipatory_contact_events_cannot_pass_the_latency_gate() {
    let mut fixture = gate0b_fixture(None);
    for row in fixture
        .rows
        .iter_mut()
        .filter(|row| row.condition == StudyCondition::IntentionalTap)
    {
        row.candidate_events[0].event_time_us =
            row.clock.truth_event_time_us.expect("tap truth") - 1;
    }
    let report = analyze_fixture(&mut fixture);

    assert_eq!(report.metrics.latency_min_us, Some(-1));
    assert_eq!(report.disposition, GateDisposition::NoGo);

    let mut gate1 = gate1_fixture();
    let classified = gate1
        .rows
        .iter_mut()
        .find(|row| {
            row.split == StudySplit::Heldout && row.condition == StudyCondition::IntentionalTap
        })
        .expect("heldout classified tap");
    classified.candidate_events[0].event_time_us =
        classified.clock.truth_event_time_us.expect("tap truth") - 1;
    let report = analyze_fixture(&mut gate1);
    assert_eq!(report.metrics.latency_min_us, Some(-1));
    assert_eq!(report.disposition, GateDisposition::NoGo);
}

#[test]
fn latency_and_class_recall_decisions_cluster_at_participant() {
    let mut latency_fixture = gate0b_fixture(None);
    for (index, row) in latency_fixture
        .rows
        .iter_mut()
        .filter(|row| {
            row.participant_id == "participant-0" && row.condition == StudyCondition::IntentionalTap
        })
        .enumerate()
    {
        if index.is_multiple_of(5) {
            row.candidate_events[0].event_time_us =
                row.clock.truth_event_time_us.expect("tap truth") + 250_000;
        }
    }
    let latency_report = analyze_fixture(&mut latency_fixture);
    assert!(latency_report.metrics.latency_p95_us.unwrap() < 100_000);
    assert!(latency_report.metrics.latency_p95_upper_95_us.unwrap() > 100_000);
    assert_eq!(latency_report.disposition, GateDisposition::Inconclusive);
    assert_eq!(
        latency_report
            .criteria
            .iter()
            .find(|criterion| criterion.id == "contact-latency-p95-upper-95")
            .expect("latency criterion")
            .outcome,
        CriterionOutcome::Inconclusive
    );

    let mut recall_fixture = gate0b_fixture(None);
    for row in recall_fixture.rows.iter_mut().filter(|row| {
        row.participant_id == "participant-0" && row.truth_label.as_deref() == Some("pinky")
    }) {
        row.candidate_events.clear();
    }
    let recall_report = analyze_fixture(&mut recall_fixture);
    assert_eq!(
        recall_report.metrics.participant_minimum_class_recall_ppm,
        Some(800_000)
    );
    assert!(
        recall_report
            .metrics
            .participant_minimum_class_recall_lower_95_ppm
            .unwrap()
            < 800_000
    );
    assert_eq!(
        recall_report
            .criteria
            .iter()
            .find(|criterion| criterion.id == "minimum-finger-recall-lower-95")
            .expect("class recall criterion")
            .outcome,
        CriterionOutcome::Inconclusive
    );
}

#[test]
fn finite_participant_latency_uncertainty_precedes_cluster_resampling() {
    let mut fixture = gate0b_fixture(None);
    for participant_index in 0..5 {
        let participant = format!("participant-{participant_index}");
        for (index, row) in fixture
            .rows
            .iter_mut()
            .filter(|row| {
                row.participant_id == participant && row.condition == StudyCondition::IntentionalTap
            })
            .enumerate()
        {
            if index < 45 {
                row.candidate_events[0].event_time_us =
                    row.clock.truth_event_time_us.expect("tap truth") + 150_000;
            }
        }
    }

    let report = analyze_fixture(&mut fixture);
    assert_eq!(report.metrics.latency_p95_us, Some(10_000));
    assert_eq!(report.metrics.latency_p95_upper_95_us, Some(150_000));
    assert_eq!(report.disposition, GateDisposition::Inconclusive);
    assert_eq!(
        report
            .criteria
            .iter()
            .find(|criterion| criterion.id == "contact-latency-p95-upper-95")
            .expect("latency criterion")
            .outcome,
        CriterionOutcome::Inconclusive
    );
}

#[test]
fn missing_mount_class_is_rejected_instead_of_reported_inconclusive() {
    let mut fixture = gate0b_fixture(None);
    for row in fixture.rows.iter_mut().filter(|row| {
        row.participant_id == "participant-0"
            && row.mount_id == "participant-0-mount-0"
            && row.truth_label.as_deref() == Some("pinky")
    }) {
        row.prompted_label = Some("index".to_owned());
        row.truth_label = Some("index".to_owned());
    }

    let (evidence, manifest, plan) = fixture.artifacts();
    let error =
        analyze_physical_study_jsonl(&evidence, &manifest, &plan).expect_err("missing class fails");
    assert!(matches!(error, PhysicalStudyError::MissingStratum(_)));
}

#[test]
fn row_hash_and_report_bytes_are_canonical() {
    let mut fixture = gate0b_fixture(None);
    let (source, manifest, plan) = fixture.artifacts();
    let report = analyze_physical_study_jsonl(&source, &manifest, &plan).expect("fixture scores");
    let bytes = report.canonical_json_bytes().expect("report serializes");
    let decoded = decode_physical_study_report(&bytes).expect("report is JSON");

    assert_eq!(decoded.schema, PHYSICAL_STUDY_REPORT_SCHEMA);
    assert_eq!(bytes.last(), Some(&b'\n'));
    assert_eq!(report.source.exact_jsonl_sha256, hash_bytes(&source));
    assert_ne!(
        report.source.exact_jsonl_sha256,
        report.source.canonical_row_set_sha256
    );
}

#[test]
fn bounded_decoders_reject_cap_plus_one_before_deserialization() {
    assert_eq!(
        validate_physical_study_artifact_size(
            PhysicalStudyArtifact::EvidenceJsonl,
            PHYSICAL_STUDY_EVIDENCE_JSONL_MAX_BYTES,
        ),
        Ok(())
    );
    assert_eq!(
        validate_physical_study_artifact_size(
            PhysicalStudyArtifact::EvidenceJsonl,
            PHYSICAL_STUDY_EVIDENCE_JSONL_MAX_BYTES + 1,
        ),
        Err(PhysicalStudyError::ArtifactTooLarge {
            artifact: PhysicalStudyArtifact::EvidenceJsonl,
            actual_bytes: PHYSICAL_STUDY_EVIDENCE_JSONL_MAX_BYTES + 1,
            maximum_bytes: PHYSICAL_STUDY_EVIDENCE_JSONL_MAX_BYTES,
        })
    );

    {
        let oversized_evidence = vec![b' '; PHYSICAL_STUDY_EVIDENCE_JSONL_MAX_BYTES + 1];
        let error = analyze_physical_study_jsonl(&oversized_evidence, b"{}", b"{}")
            .expect_err("oversized evidence must fail before parsing");
        assert_eq!(
            error,
            PhysicalStudyError::ArtifactTooLarge {
                artifact: PhysicalStudyArtifact::EvidenceJsonl,
                actual_bytes: PHYSICAL_STUDY_EVIDENCE_JSONL_MAX_BYTES + 1,
                maximum_bytes: PHYSICAL_STUDY_EVIDENCE_JSONL_MAX_BYTES,
            }
        );
    }

    let maximum_row = vec![b' '; PHYSICAL_STUDY_EVIDENCE_ROW_MAX_BYTES];
    assert!(matches!(
        decode_physical_study_jsonl(&maximum_row),
        Err(PhysicalStudyError::InvalidJson { line: 1, .. })
    ));
    let oversized_row = vec![b' '; PHYSICAL_STUDY_EVIDENCE_ROW_MAX_BYTES + 1];
    let expected_row_error = PhysicalStudyError::EvidenceRowTooLarge {
        line: 1,
        actual_bytes: PHYSICAL_STUDY_EVIDENCE_ROW_MAX_BYTES + 1,
        maximum_bytes: PHYSICAL_STUDY_EVIDENCE_ROW_MAX_BYTES,
    };
    assert_eq!(
        decode_physical_study_jsonl(&oversized_row).expect_err("oversized JSONL row must fail"),
        expected_row_error
    );
    assert_eq!(
        decode_physical_study_evidence_row(&oversized_row)
            .expect_err("oversized row document must fail"),
        expected_row_error
    );

    {
        let oversized_manifest = vec![b' '; PHYSICAL_STUDY_SPLIT_MANIFEST_MAX_BYTES + 1];
        assert_eq!(
            decode_physical_study_split_manifest(&oversized_manifest)
                .expect_err("oversized manifest must fail"),
            PhysicalStudyError::ArtifactTooLarge {
                artifact: PhysicalStudyArtifact::SplitManifest,
                actual_bytes: PHYSICAL_STUDY_SPLIT_MANIFEST_MAX_BYTES + 1,
                maximum_bytes: PHYSICAL_STUDY_SPLIT_MANIFEST_MAX_BYTES,
            }
        );
    }

    let oversized_plan = vec![b' '; PHYSICAL_STUDY_ANALYSIS_PLAN_MAX_BYTES + 1];
    assert_eq!(
        decode_physical_study_analysis_plan(&oversized_plan).expect_err("oversized plan must fail"),
        PhysicalStudyError::ArtifactTooLarge {
            artifact: PhysicalStudyArtifact::AnalysisPlan,
            actual_bytes: PHYSICAL_STUDY_ANALYSIS_PLAN_MAX_BYTES + 1,
            maximum_bytes: PHYSICAL_STUDY_ANALYSIS_PLAN_MAX_BYTES,
        }
    );
    let oversized_output_plan = PhysicalStudyAnalysisPlan::v1(
        PhysicalStudyGate::Gate0B,
        "x".repeat(PHYSICAL_STUDY_ANALYSIS_PLAN_MAX_BYTES),
        hash("manifest"),
        FrozenStudyPipeline {
            contact_detector_sha256: hash("detector"),
            contact_detector_fit_splits: vec![StudySplit::Train],
            observation_representation: ObservationRepresentation::Derived,
            transform_sha256: Some(hash("transform")),
            transform_fit_splits: vec![StudySplit::Train],
        },
    );
    assert!(matches!(
        oversized_output_plan.canonical_json_bytes(),
        Err(PhysicalStudyError::ArtifactTooLarge {
            artifact: PhysicalStudyArtifact::AnalysisPlan,
            actual_bytes,
            maximum_bytes: PHYSICAL_STUDY_ANALYSIS_PLAN_MAX_BYTES,
        }) if actual_bytes > PHYSICAL_STUDY_ANALYSIS_PLAN_MAX_BYTES
    ));

    let oversized_report = vec![b' '; PHYSICAL_STUDY_REPORT_MAX_BYTES + 1];
    assert_eq!(
        decode_physical_study_report(&oversized_report).expect_err("oversized report must fail"),
        PhysicalStudyError::ArtifactTooLarge {
            artifact: PhysicalStudyArtifact::Report,
            actual_bytes: PHYSICAL_STUDY_REPORT_MAX_BYTES + 1,
            maximum_bytes: PHYSICAL_STUDY_REPORT_MAX_BYTES,
        }
    );
}

#[test]
fn single_row_decoder_accepts_pretty_unsealed_collection_input() {
    let fixture = gate0b_fixture(None);
    let expected = &fixture.rows[0];
    let pretty = serde_json::to_vec_pretty(expected).expect("fixture row serializes");
    assert!(pretty.contains(&b'\n'));

    let decoded = decode_physical_study_evidence_row(&pretty).expect("pretty row decodes");
    assert_eq!(&decoded, expected);
    assert!(decoded.row_sha256.is_empty());
}

#[test]
fn row_sealing_and_encoding_bound_constructed_feature_output() {
    let mut fixture = Fixture::new();
    fixture.push(FixtureRow {
        gate: PhysicalStudyGate::Gate0B,
        split: StudySplit::Heldout,
        participant: "participant",
        mount: "mount",
        session: "session",
        hand: Hand::Left,
        condition: StudyCondition::IntentionalTap,
        label: Some("index"),
        features: vec![i32::MAX; 100_000],
        miss_every: None,
    });
    let row = &mut fixture.rows[0];
    assert!(matches!(
        seal_physical_study_evidence_row(row),
        Err(PhysicalStudyError::EvidenceRowTooLarge {
            line: 1,
            actual_bytes,
            maximum_bytes: PHYSICAL_STUDY_EVIDENCE_ROW_MAX_BYTES,
        }) if actual_bytes > PHYSICAL_STUDY_EVIDENCE_ROW_MAX_BYTES
    ));
    assert!(matches!(
        encode_physical_study_jsonl(&fixture.rows),
        Err(PhysicalStudyError::EvidenceRowTooLarge {
            line: 1,
            actual_bytes,
            maximum_bytes: PHYSICAL_STUDY_EVIDENCE_ROW_MAX_BYTES,
        }) if actual_bytes > PHYSICAL_STUDY_EVIDENCE_ROW_MAX_BYTES
    ));
}

#[test]
fn sealing_rejects_a_hash_that_pushes_the_final_row_over_the_cap() {
    let mut row = evidence_row_with_empty_hash_size(PHYSICAL_STUDY_EVIDENCE_ROW_MAX_BYTES);

    assert_eq!(row.row_sha256, "");
    assert_eq!(
        seal_physical_study_evidence_row(&mut row).expect_err("sealed row must also fit"),
        PhysicalStudyError::EvidenceRowTooLarge {
            line: 1,
            actual_bytes: PHYSICAL_STUDY_EVIDENCE_ROW_MAX_BYTES + 64,
            maximum_bytes: PHYSICAL_STUDY_EVIDENCE_ROW_MAX_BYTES,
        }
    );
    assert_eq!(row.row_sha256, "", "failed sealing must be transactional");
}

#[test]
fn jsonl_encoder_reports_the_exact_full_size_after_crossing_the_cap() {
    let mut row = evidence_row_with_empty_hash_size(PHYSICAL_STUDY_EVIDENCE_ROW_MAX_BYTES - 64);
    seal_physical_study_evidence_row(&mut row).expect("maximum-size sealed row fits");
    assert_eq!(
        serde_json::to_vec(&row)
            .expect("maximum-size row serializes")
            .len(),
        PHYSICAL_STUDY_EVIDENCE_ROW_MAX_BYTES
    );

    let bytes_per_line = PHYSICAL_STUDY_EVIDENCE_ROW_MAX_BYTES + 1;
    let row_count = PHYSICAL_STUDY_EVIDENCE_JSONL_MAX_BYTES / bytes_per_line + 2;
    let exact_full_size = row_count * bytes_per_line;
    let rows = vec![row; row_count];
    assert_eq!(
        encode_physical_study_jsonl(&rows).expect_err("full JSONL must exceed its cap"),
        PhysicalStudyError::ArtifactTooLarge {
            artifact: PhysicalStudyArtifact::EvidenceJsonl,
            actual_bytes: exact_full_size,
            maximum_bytes: PHYSICAL_STUDY_EVIDENCE_JSONL_MAX_BYTES,
        }
    );
}

fn evidence_row_with_empty_hash_size(target_bytes: usize) -> PhysicalStudyEvidenceRow {
    let mut fixture = Fixture::new();
    fixture.push(FixtureRow {
        gate: PhysicalStudyGate::Gate0B,
        split: StudySplit::Heldout,
        participant: "participant",
        mount: "mount",
        session: "session",
        hand: Hand::Left,
        condition: StudyCondition::IntentionalTap,
        label: Some("index"),
        features: vec![i32::MIN; 80_000],
        miss_every: None,
    });
    let mut row = fixture.rows.remove(0);
    // Large integer features keep the in-memory fixture much smaller than its
    // canonical JSON while the final ASCII padding makes the boundary exact.
    let encoded_bytes = serde_json::to_vec(&row)
        .expect("boundary fixture serializes")
        .len();
    assert!(encoded_bytes < target_bytes);
    row.rig_id
        .push_str(&"x".repeat(target_bytes - encoded_bytes));
    assert_eq!(
        serde_json::to_vec(&row)
            .expect("padded boundary fixture serializes")
            .len(),
        target_bytes
    );
    row
}

fn hash_bytes(bytes: &[u8]) -> String {
    format_sha256(&sha256(bytes))
}

#[test]
fn finger_fixture_covers_both_declared_hands() {
    let fixture = gate1_fixture();
    for hand in [Hand::Left, Hand::Right] {
        for finger in FINGERS {
            assert!(fixture.rows.iter().any(|row| {
                row.split == StudySplit::Heldout
                    && row.hand == hand
                    && row.truth_label.as_deref() == Some(finger)
            }));
        }
    }
}
