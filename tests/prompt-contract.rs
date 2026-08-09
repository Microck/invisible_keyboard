use std::collections::BTreeSet;

use invisible_keyboard::{
    layout::{
        Hand, PHYSICAL_POSITION_COUNT, PhysicalPosition, all_slots, keyboard_warm_start_eligible,
        position_index,
    },
    prompts::{
        HELD_OUT_ORDINARY_MOTION_EXPOSURE_MS, HeldOutSurfacePlan, PASSIVE_THRESHOLD_TARGET_COUNT,
        PROMPT_CORPUS_VERSION, PromptCorpus, PromptKind, SURFACE_POSITION_REPETITIONS,
        SURFACE_REARM_IDLE_WINDOWS, SURFACE_RESPONSE_WINDOW_END_NS,
        SURFACE_RESPONSE_WINDOW_START_NS, SURFACE_TARGET_COUNT, SurfacePlanDomain,
        SurfacePromptSegment, SurfaceSegmentKind, SurfaceTrainingPlan,
    },
};

#[test]
fn frozen_corpus_covers_every_keyboard_warm_start_position() {
    let corpus = PromptCorpus::v1();
    let prompts = corpus.prompts();
    let mut counts = [0_u16; PHYSICAL_POSITION_COUNT];
    for prompt in prompts {
        for position in prompt.expected_positions {
            let index = all_slots()
                .iter()
                .position(|slot| slot.position == position)
                .expect("prompt position belongs to Core-28 geometry");
            counts[index] += 1;
        }
    }

    for (index, slot) in all_slots().iter().enumerate() {
        assert_eq!(
            counts[index] > 0,
            keyboard_warm_start_eligible(slot.position),
            "unexpected prompt coverage for {}",
            slot.label
        );
    }
}

#[test]
fn corpus_has_natural_coverage_and_reserved_exercises() {
    let prompts = PromptCorpus::v1().prompts();

    for kind in [
        PromptKind::Natural,
        PromptKind::Coverage,
        PromptKind::Reserved,
    ] {
        assert!(prompts.iter().any(|prompt| prompt.kind == kind));
    }
}

#[test]
fn prompt_selection_is_deterministic_and_targets_the_largest_deficit() {
    let corpus = PromptCorpus::v1();
    let mut deficits = [0_u16; PHYSICAL_POSITION_COUNT];
    let q_index = all_slots()
        .iter()
        .position(|slot| slot.label == "Q")
        .expect("Q exists");
    deficits[q_index] = 20;

    let first = corpus.next_keyboard_prompt(&deficits, 3);
    let second = corpus.next_keyboard_prompt(&deficits, 3);
    assert_eq!(first, second);
    assert!(
        first
            .expected_positions
            .contains(&all_slots()[q_index].position)
    );
}

#[test]
fn corpus_identity_is_stable_and_content_is_ascii() {
    let corpus = PromptCorpus::v1();
    assert_eq!(corpus.version(), PROMPT_CORPUS_VERSION);
    assert_eq!(corpus.sha256(), PromptCorpus::v1().sha256());
    assert!(corpus.canonical_bytes().is_ascii());

    let rendered = String::from_utf8(corpus.canonical_bytes()).expect("corpus is UTF-8");
    for forbidden in ["angle", "switch travel", "password", "free-form"] {
        assert!(!rendered.to_ascii_lowercase().contains(forbidden));
    }
}

#[test]
fn surface_training_plan_is_frozen_in_manifest_order() {
    let plan = SurfaceTrainingPlan::v1();

    assert_eq!(plan.domain(), SurfacePlanDomain::Training);
    assert_eq!(
        plan.timing().response_window_start_ns(),
        SURFACE_RESPONSE_WINDOW_START_NS
    );
    assert_eq!(
        plan.timing().response_window_end_ns(),
        SURFACE_RESPONSE_WINDOW_END_NS
    );
    assert_eq!(SURFACE_RESPONSE_WINDOW_END_NS, 1_500_000_000);
    assert_eq!(
        plan.timing().rearm_idle_windows(),
        SURFACE_REARM_IDLE_WINDOWS
    );

    let canonical_positions = all_slots()
        .iter()
        .map(|slot| slot.position)
        .collect::<Vec<_>>();
    let expected_positions = (0..SURFACE_POSITION_REPETITIONS)
        .flat_map(|_| canonical_positions.iter().copied())
        .collect::<Vec<_>>();
    assert_eq!(
        &target_positions(plan.scoring_targets())[..SURFACE_TARGET_COUNT],
        expected_positions.as_slice()
    );
    assert_eq!(
        plan.scoring_targets().len(),
        SURFACE_TARGET_COUNT + PASSIVE_THRESHOLD_TARGET_COUNT
    );
    assert_unique_prompt_ids(plan.segments());
    assert_target_metadata_is_a_join_table(plan.segments(), plan.scoring_targets());
    assert_non_overlapping_schedule(plan.segments());
}

#[test]
fn training_plan_roundtrips_but_rejects_canonical_mutations() {
    let plan = SurfaceTrainingPlan::v1();
    let canonical = plan.canonical_bytes().expect("training plan serializes");
    let parsed = SurfaceTrainingPlan::from_json(&canonical).expect("training plan parses");

    assert_eq!(parsed, plan);
    assert_eq!(
        parsed.canonical_bytes().expect("parsed plan serializes"),
        canonical
    );
    assert_eq!(
        parsed.sha256().expect("training plan hashes"),
        plan.sha256().expect("training plan hashes")
    );

    let first_id = plan.segments()[0].id().as_str();
    let mutated = replace_once(&canonical, first_id, "surface-training-v1-mutated-id");
    assert!(SurfaceTrainingPlan::from_json(&mutated).is_err());

    let reordered = replace_once(&canonical, "\"target_ordinal\":0", "\"target_ordinal\":1");
    assert!(SurfaceTrainingPlan::from_json(&reordered).is_err());
}

#[test]
fn held_out_plan_is_artifact_bound_deterministic_and_balanced() {
    let artifact_sha256 = [0x3a; 32];
    let run_seed = 0x4b5c_6d7e_8f90_a1b2;
    let plan = HeldOutSurfacePlan::v1(artifact_sha256, run_seed);
    let same = HeldOutSurfacePlan::v1(artifact_sha256, run_seed);
    let different_artifact = HeldOutSurfacePlan::v1([0x3b; 32], run_seed);
    let different_run = HeldOutSurfacePlan::v1(artifact_sha256, run_seed + 1);

    assert_eq!(plan, same);
    assert_eq!(plan.domain(), SurfacePlanDomain::HeldOutValidation);
    assert_eq!(plan.locked_artifact_sha256(), artifact_sha256);
    assert_eq!(plan.run_seed(), run_seed);
    assert_ne!(
        plan.canonical_bytes().expect("held-out plan serializes"),
        different_artifact
            .canonical_bytes()
            .expect("different artifact plan serializes")
    );
    assert_ne!(
        plan.canonical_bytes().expect("held-out plan serializes"),
        different_run
            .canonical_bytes()
            .expect("different run plan serializes")
    );
    assert_ne!(
        target_positions(plan.scoring_targets()),
        target_positions(different_artifact.scoring_targets())
    );
    assert_ne!(
        target_positions(plan.scoring_targets()),
        target_positions(different_run.scoring_targets())
    );

    let mut counts = [0_usize; PHYSICAL_POSITION_COUNT];
    for target in plan.scoring_targets() {
        counts[position_index(target.position()).expect("target belongs to the manifest")] += 1;
    }
    assert_eq!(
        counts,
        [SURFACE_POSITION_REPETITIONS; PHYSICAL_POSITION_COUNT]
    );

    for round in plan.scoring_targets().chunks_exact(PHYSICAL_POSITION_COUNT) {
        let positions = round
            .iter()
            .map(|target| target.position())
            .collect::<BTreeSet<_>>();
        assert_eq!(positions.len(), PHYSICAL_POSITION_COUNT);
        for pair in round.windows(2) {
            assert_ne!(pair[0].position().hand, pair[1].position().hand);
        }
    }

    assert_unique_prompt_ids(plan.segments());
    assert_target_metadata_is_a_join_table(plan.segments(), plan.scoring_targets());
    assert_non_overlapping_schedule(plan.segments());
}

#[test]
fn held_out_null_intervals_are_explicit_and_meet_the_provisional_gate() {
    let plan = HeldOutSurfacePlan::v1([0xa5; 32], 91);
    let derived_ordinary_motion_ms_by_hand = [Hand::Left, Hand::Right].map(|hand| {
        plan.segments()
            .iter()
            .filter(|segment| {
                segment.kind() == SurfaceSegmentKind::OrdinaryMotion
                    && segment.motion_hand() == Some(hand)
            })
            .map(SurfacePromptSegment::duration_ns)
            .sum::<u64>()
            / 1_000_000
    });
    let motion_hands = plan
        .segments()
        .iter()
        .filter(|segment| segment.kind() == SurfaceSegmentKind::OrdinaryMotion)
        .filter_map(SurfacePromptSegment::motion_hand)
        .collect::<BTreeSet<_>>();

    assert!(
        derived_ordinary_motion_ms_by_hand
            .iter()
            .all(|exposure| *exposure >= HELD_OUT_ORDINARY_MOTION_EXPOSURE_MS)
    );
    assert_eq!(
        plan.ordinary_motion_exposure_ms(),
        *derived_ordinary_motion_ms_by_hand
            .iter()
            .min()
            .expect("two hand exposures exist")
    );
    assert!(plan.null_exposure_ms() >= HELD_OUT_ORDINARY_MOTION_EXPOSURE_MS);
    assert_eq!(motion_hands, BTreeSet::from([Hand::Left, Hand::Right]));
    assert!(
        plan.segments()
            .iter()
            .any(|segment| segment.kind() == SurfaceSegmentKind::QuietNull)
    );
}

#[test]
fn held_out_roundtrip_requires_the_original_artifact_and_seed() {
    let artifact_sha256 = [0x6c; 32];
    let run_seed = 7331;
    let plan = HeldOutSurfacePlan::v1(artifact_sha256, run_seed);
    let canonical = plan.canonical_bytes().expect("held-out plan serializes");

    let parsed = HeldOutSurfacePlan::from_json(&canonical, artifact_sha256, run_seed)
        .expect("artifact-bound plan parses");
    assert_eq!(parsed, plan);
    assert_eq!(
        parsed.canonical_bytes().expect("parsed plan serializes"),
        canonical
    );
    assert!(HeldOutSurfacePlan::from_json(&canonical, [0x6d; 32], run_seed).is_err());
    assert!(HeldOutSurfacePlan::from_json(&canonical, artifact_sha256, run_seed + 1).is_err());

    let ordinary = plan
        .segments()
        .iter()
        .find(|segment| segment.kind() == SurfaceSegmentKind::OrdinaryMotion)
        .expect("held-out plan contains ordinary motion");
    let overlap = replace_once(
        &canonical,
        &format!("\"start_ns\":{}", ordinary.start_ns()),
        &format!("\"start_ns\":{}", plan.segments()[0].start_ns()),
    );
    assert!(HeldOutSurfacePlan::from_json(&overlap, artifact_sha256, run_seed).is_err());
}

fn target_positions(
    targets: &[invisible_keyboard::prompts::SurfaceTargetMetadata],
) -> Vec<PhysicalPosition> {
    targets.iter().map(|target| target.position()).collect()
}

fn assert_unique_prompt_ids(segments: &[SurfacePromptSegment]) {
    let ids = segments
        .iter()
        .map(|segment| segment.id().as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(ids.len(), segments.len());
}

fn assert_target_metadata_is_a_join_table(
    segments: &[SurfacePromptSegment],
    targets: &[invisible_keyboard::prompts::SurfaceTargetMetadata],
) {
    let prompted_ids = segments
        .iter()
        .filter(|segment| {
            matches!(
                segment.kind(),
                SurfaceSegmentKind::PromptedTap | SurfaceSegmentKind::ThresholdTap
            )
        })
        .map(|segment| segment.id())
        .collect::<Vec<_>>();
    let scoring_ids = targets
        .iter()
        .map(|target| target.prompt_id())
        .collect::<Vec<_>>();
    assert_eq!(prompted_ids, scoring_ids);

    // Capture/presentation segments carry IDs and timing only. Position truth
    // appears solely in the scoring join table after the serialized segments.
    let plan = HeldOutSurfacePlan::v1([0xf0; 32], 17);
    let rendered = String::from_utf8(
        plan.canonical_bytes()
            .expect("held-out plan serializes as UTF-8"),
    )
    .expect("canonical JSON is UTF-8");
    let scoring_offset = rendered
        .find("\"scoring_targets\"")
        .expect("canonical plan has scoring metadata");
    assert!(!rendered[..scoring_offset].contains("\"position\":"));
}

fn assert_non_overlapping_schedule(segments: &[SurfacePromptSegment]) {
    for pair in segments.windows(2) {
        assert!(pair[0].end_ns() <= pair[1].start_ns());
    }

    let null_intervals = segments
        .iter()
        .filter(|segment| {
            !matches!(
                segment.kind(),
                SurfaceSegmentKind::PromptedTap | SurfaceSegmentKind::ThresholdTap
            )
        })
        .map(SurfacePromptSegment::interval)
        .collect::<Vec<_>>();
    for prompted in segments.iter().filter(|segment| {
        matches!(
            segment.kind(),
            SurfaceSegmentKind::PromptedTap | SurfaceSegmentKind::ThresholdTap
        )
    }) {
        let response = prompted
            .response_interval()
            .expect("prompted tap has a response interval");
        let rearm = prompted
            .rearm_interval()
            .expect("prompted tap has a rearm interval");
        assert!(prompted.start_ns() <= response.start_ns());
        assert!(prompted.end_ns() >= rearm.end_ns());
        assert_eq!(response.end_ns(), rearm.start_ns());
        for interval in &null_intervals {
            assert!(!interval.overlaps(response));
            assert!(!interval.overlaps(rearm));
        }
    }
}

fn replace_once(source: &[u8], from: &str, to: &str) -> Vec<u8> {
    let source = String::from_utf8(source.to_vec()).expect("canonical plan JSON is UTF-8");
    assert!(source.contains(from), "mutation source must exist: {from}");
    source.replacen(from, to, 1).into_bytes()
}
