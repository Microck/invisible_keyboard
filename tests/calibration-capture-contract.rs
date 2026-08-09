use invisible_keyboard::{
    artifact::{KeyboardIdentity, KeyboardLayoutConfirmationReceipt},
    calibration_capture::{
        CALIBRATION_MATCHING_RULE_SCHEMA, CONTACT_DEBOUNCE_WINDOWS, CONTACT_FEATURE_SCHEMA,
        CONTACT_REARM_WINDOWS, CONTACT_RESPONSE_END_NS, CONTACT_RESPONSE_START_NS,
        CalibrationCaptureError, CalibrationCaptureInput, CalibrationCaptureJournal,
        CalibrationMatchingRule, CalibrationSurfacePlanBinding, CapturedCalibrationEvent,
        HandContactThresholds, SurfacePresentationReceipt,
    },
    calibration_engine::{EvidenceDomain, EvidencePurpose, EvidenceRole},
    labels::{RawKeyState, RawKeyboardEvent},
    layout::{Hand, KeyUsage, position_for_usage},
    manifest::format_sha256,
    neutral::{NeutralRecordWindow, NeutralWindowRole},
    prompts::{
        HeldOutSurfacePlan, SurfacePlanError, SurfacePromptSegment, SurfaceSegmentKind,
        SurfaceTrainingPlan,
    },
};

const SESSION_ID: [u8; 16] = [0x53; 16];
const DEVICE_ID: &str = "/dev/input/event-fixture";
const ARTIFACT_SHA256: [u8; 32] = [0xa7; 32];
const HELD_OUT_SEED: u64 = 0x51_7f;
const SURFACE_PLAN_STARTED_NS: u64 = 1_000_000_000;

#[test]
fn passive_surface_receipts_require_one_monotonic_rendered_revision() {
    for receipt in [
        SurfacePresentationReceipt::new(2, 1, 3, 1),
        SurfacePresentationReceipt::new(1, 3, 2, 1),
        SurfacePresentationReceipt::new(1, 2, 3, 0),
    ] {
        assert!(matches!(
            receipt,
            Err(CalibrationCaptureError::InvalidSurfacePresentation)
        ));
    }

    let receipt =
        SurfacePresentationReceipt::new(1, 2, 3, 4).expect("ordered render provenance is accepted");
    assert_eq!(receipt.planned_host_time_ns(), 1);
    assert_eq!(receipt.published_host_time_ns(), 2);
    assert_eq!(receipt.acknowledged_host_time_ns(), 3);
    assert_eq!(receipt.instruction_revision(), 4);
}

#[test]
fn canonical_fitting_and_held_out_facts_round_trip_and_are_complete() {
    for journal in [fitting_journal(), held_out_journal()] {
        journal
            .validate_complete_surface_plan()
            .expect("every required prompt fact is present in canonical order");
        let bytes = journal.canonical_json().expect("journal serializes");
        assert_eq!(
            CalibrationCaptureJournal::from_json(&bytes).expect("journal parses"),
            journal
        );
    }
}

#[test]
fn held_out_keyboard_activity_is_state_only_keyboard_provenance() {
    let event = RawKeyboardEvent {
        device_id: DEVICE_ID.to_owned(),
        physical_code: 30,
        position: None,
        modifier: false,
        state: RawKeyState::Down,
        kernel_time_ns: None,
        host_time_ns: u64::MAX,
    };
    let mut state_only = held_out_journal();
    state_only
        .events
        .push(CapturedCalibrationEvent::KeyboardStateOnly {
            role: EvidenceRole::HeldOutValidation,
            domain: EvidenceDomain::Keyboard,
            block_id: 40,
            event: event.clone(),
        });
    state_only
        .validate()
        .expect("held-out capture may retain state-only selected-keyboard activity");

    let mut candidate = held_out_journal();
    candidate.events.push(CapturedCalibrationEvent::Keyboard {
        role: EvidenceRole::HeldOutValidation,
        domain: EvidenceDomain::Keyboard,
        block_id: 40,
        event,
    });
    assert!(matches!(
        candidate.validate(),
        Err(CalibrationCaptureError::KeyboardEventProvenanceMismatch)
    ));
}

#[test]
fn first_canonical_fitting_quiet_id_resolves_and_completes() {
    let plan = SurfaceTrainingPlan::v1();
    let first_quiet = plan
        .segments()
        .iter()
        .find(|segment| segment.kind() == SurfaceSegmentKind::QuietNull)
        .expect("fitting plan contains inter-prompt quiet")
        .id();
    assert_eq!(first_quiet.as_str(), "surface-training-v1-0001");

    let journal = fitting_journal();
    assert_eq!(
        journal
            .events
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    CapturedCalibrationEvent::Idle { prompt_id, .. } if prompt_id == first_quiet
                )
            })
            .count(),
        2,
        "both hands are recorded as facts under the canonical quiet prompt"
    );
    journal
        .validate_complete_surface_plan()
        .expect("the canonical quiet ID resolves through the fitting plan");
}

#[test]
fn surface_facts_reject_unknown_duplicate_missing_and_reordered_prompt_ids() {
    let plan = SurfaceTrainingPlan::v1();

    let mut unknown = fitting_events(&plan);
    let foreign_id = HeldOutSurfacePlan::v1(ARTIFACT_SHA256, HELD_OUT_SEED).segments()[0]
        .id()
        .clone();
    let CapturedCalibrationEvent::DisplayAcknowledged { prompt_id, .. } = &mut unknown[0] else {
        panic!("the fitting plan starts with a target");
    };
    *prompt_id = foreign_id.clone();
    assert!(matches!(
        CalibrationCaptureJournal::new(fitting_input(unknown)),
        Err(CalibrationCaptureError::UnknownSurfacePrompt(actual)) if actual == foreign_id.as_str()
    ));

    let mut duplicate = fitting_events(&plan);
    let duplicate_event = duplicate
        .iter()
        .rev()
        .find(|event| matches!(event, CapturedCalibrationEvent::DisplayAcknowledged { .. }))
        .expect("fitting plan contains targets")
        .clone();
    duplicate.push(duplicate_event);
    assert!(matches!(
        CalibrationCaptureJournal::new(fitting_input(duplicate)),
        Err(CalibrationCaptureError::DuplicateSurfacePrompt(_))
    ));

    let mut missing = fitting_events(&plan);
    let missing_id = match missing.pop().expect("fitting plan contains facts") {
        CapturedCalibrationEvent::Idle { prompt_id, .. }
        | CapturedCalibrationEvent::DisplayAcknowledged { prompt_id, .. } => prompt_id,
        CapturedCalibrationEvent::Keyboard { .. }
        | CapturedCalibrationEvent::KeyboardStateOnly { .. } => {
            panic!("the fitting fixture ends with a surface fact")
        }
    };
    let journal = CalibrationCaptureJournal::new(fitting_input(missing))
        .expect("an in-progress canonical prefix remains a valid journal");
    assert!(matches!(
        journal.validate_complete_surface_plan(),
        Err(CalibrationCaptureError::MissingSurfacePrompt(actual)) if actual == missing_id.as_str()
    ));

    let mut reordered = fitting_events(&plan);
    let expected = plan.segments()[0].id().as_str().to_owned();
    let actual = plan.segments()[2].id().as_str().to_owned();
    let first_target = display_index_for_prompt(&reordered, &expected);
    let second_target = display_index_for_prompt(&reordered, &actual);
    reordered.swap(first_target, second_target);
    assert!(matches!(
        CalibrationCaptureJournal::new(fitting_input(reordered)),
        Err(CalibrationCaptureError::ReorderedSurfacePrompt {
            expected: reported_expected,
            actual: reported_actual,
        }) if reported_expected == expected && reported_actual == actual
    ));
}

#[test]
fn surface_facts_reject_nonincreasing_prompt_time_and_revision() {
    let plan = SurfaceTrainingPlan::v1();
    let canonical = fitting_events(&plan);
    let target_indices = display_indices(&canonical);

    let mut mistimed = canonical.clone();
    let first = presentation_at(&mistimed, target_indices[0]);
    let second = presentation_at(&mistimed, target_indices[1]);
    replace_presentation(
        &mut mistimed,
        target_indices[1],
        SurfacePresentationReceipt::new(
            first.planned_host_time_ns(),
            second.published_host_time_ns(),
            second.acknowledged_host_time_ns(),
            second.instruction_revision(),
        )
        .expect("the individual receipt remains ordered"),
    );
    assert!(matches!(
        CalibrationCaptureJournal::new(fitting_input(mistimed)),
        Err(CalibrationCaptureError::SurfacePromptTimingMismatch(_))
    ));

    let mut stale_revision = canonical;
    let first = presentation_at(&stale_revision, target_indices[0]);
    let second = presentation_at(&stale_revision, target_indices[1]);
    replace_presentation(
        &mut stale_revision,
        target_indices[1],
        SurfacePresentationReceipt::new(
            second.planned_host_time_ns(),
            second.published_host_time_ns(),
            second.acknowledged_host_time_ns(),
            first.instruction_revision(),
        )
        .expect("the individual receipt remains ordered"),
    );
    assert!(matches!(
        CalibrationCaptureJournal::new(fitting_input(stale_revision)),
        Err(CalibrationCaptureError::SurfacePromptRevisionMismatch(_))
    ));
}

#[test]
fn compressed_surface_timeline_is_rejected_even_when_each_receipt_is_internally_ordered() {
    let plan = SurfaceTrainingPlan::v1();
    let mut compressed = fitting_events(&plan);
    let third_display = display_indices(&compressed)[2];
    let original = presentation_at(&compressed, third_display);
    let compression_ns = 100_000_000;
    replace_presentation(
        &mut compressed,
        third_display,
        SurfacePresentationReceipt::new(
            original.planned_host_time_ns() - compression_ns,
            original.published_host_time_ns() - compression_ns,
            original.acknowledged_host_time_ns() - compression_ns,
            original.instruction_revision(),
        )
        .expect("the compressed receipt is internally ordered"),
    );

    assert!(matches!(
        CalibrationCaptureJournal::new(fitting_input(compressed)),
        Err(CalibrationCaptureError::SurfacePromptTimingMismatch(_))
    ));
}

#[test]
fn same_finger_a_to_q_prompt_relabel_is_rejected_by_plan_order() {
    let plan = SurfaceTrainingPlan::v1();
    let a = position_for_usage(KeyUsage::A).expect("A belongs to Core-28");
    let q = position_for_usage(KeyUsage::Q).expect("Q belongs to Core-28");
    assert_eq!(a.hand, q.hand);
    assert_eq!(
        a.finger, q.finger,
        "A and Q exercise same-finger relabeling"
    );

    let a_id = plan
        .scoring_targets()
        .iter()
        .find(|target| target.position() == a)
        .expect("the plan targets A")
        .prompt_id()
        .clone();
    let q_id = plan
        .scoring_targets()
        .iter()
        .find(|target| target.position() == q)
        .expect("the plan targets Q")
        .prompt_id()
        .clone();
    let mut events = fitting_events(&plan);
    let a_event = events
        .iter_mut()
        .find(|event| {
            matches!(
                event,
                CapturedCalibrationEvent::DisplayAcknowledged { prompt_id, .. } if prompt_id == &a_id
            )
        })
        .expect("the plan emits the A prompt");
    let CapturedCalibrationEvent::DisplayAcknowledged { prompt_id, .. } = a_event else {
        unreachable!("the search selected a prompted tap")
    };
    *prompt_id = q_id.clone();

    assert!(matches!(
        CalibrationCaptureJournal::new(fitting_input(events)),
        Err(CalibrationCaptureError::ReorderedSurfacePrompt { expected, actual })
            if expected == a_id.as_str() && actual == q_id.as_str()
    ));
}

#[test]
fn surface_fact_schema_has_no_role_block_position_null_or_hand_channel() {
    let journal = held_out_journal();
    let value: serde_json::Value =
        serde_json::from_slice(&journal.canonical_json().expect("journal serializes"))
            .expect("canonical journal is JSON");
    let events = value["events"].as_array().expect("events are an array");

    for event in events.iter().filter(|event| {
        matches!(
            event["kind"].as_str(),
            Some("display-acknowledged" | "idle")
        )
    }) {
        for forbidden in [
            "role",
            "domain",
            "block_id",
            "position",
            "null_kind",
            "hand",
        ] {
            assert!(
                event.get(forbidden).is_none(),
                "surface fact unexpectedly exposes {forbidden}: {event}"
            );
        }
    }
}

#[test]
fn parser_rejects_role_block_and_null_kind_injection() {
    let canonical = held_out_journal();
    let source: serde_json::Value =
        serde_json::from_slice(&canonical.canonical_json().expect("journal serializes"))
            .expect("canonical journal is JSON");

    for (kind, field, value) in [
        (
            "display-acknowledged",
            "role",
            serde_json::json!("training"),
        ),
        ("display-acknowledged", "block_id", serde_json::json!(999)),
        ("display-acknowledged", "position", serde_json::json!("A")),
        ("idle", "null_kind", serde_json::json!("ordinary-motion")),
        ("idle", "hand", serde_json::json!("right")),
    ] {
        let mut injected = source.clone();
        let event = injected["events"]
            .as_array_mut()
            .expect("events are an array")
            .iter_mut()
            .find(|event| event["kind"] == kind)
            .expect("fixture contains the requested surface fact")
            .as_object_mut()
            .expect("surface fact is an object");
        event.insert(field.to_owned(), value);
        let bytes = serde_json::to_vec(&injected).expect("tampered journal serializes");
        assert!(matches!(
            CalibrationCaptureJournal::from_json(&bytes),
            Err(CalibrationCaptureError::Json(_))
        ));
    }
}

#[test]
fn quiet_prompt_cannot_be_reclassified_as_ordinary_motion() {
    let plan = HeldOutSurfacePlan::v1(ARTIFACT_SHA256, HELD_OUT_SEED);
    let quiet_id = plan
        .segments()
        .iter()
        .find(|segment| segment.kind() == SurfaceSegmentKind::QuietNull)
        .expect("held-out plan contains quiet nulls")
        .id()
        .as_str()
        .to_owned();
    let journal = held_out_journal();
    let mut value: serde_json::Value =
        serde_json::from_slice(&journal.canonical_json().expect("journal serializes"))
            .expect("canonical journal is JSON");
    let quiet = value["events"]
        .as_array_mut()
        .expect("events are an array")
        .iter_mut()
        .find(|event| event["prompt_id"] == quiet_id)
        .expect("quiet fact is present")
        .as_object_mut()
        .expect("quiet fact is an object");
    quiet.insert("null_kind".to_owned(), serde_json::json!("ordinary-motion"));

    assert!(matches!(
        CalibrationCaptureJournal::from_json(
            &serde_json::to_vec(&value).expect("tampered journal serializes")
        ),
        Err(CalibrationCaptureError::Json(_))
    ));
}

#[test]
fn purpose_plan_artifact_and_seed_bindings_are_exact() {
    let held_out_plan = HeldOutSurfacePlan::v1(ARTIFACT_SHA256, HELD_OUT_SEED);
    let held_out_binding =
        CalibrationSurfacePlanBinding::held_out(&held_out_plan, SURFACE_PLAN_STARTED_NS)
            .expect("held-out plan binds");

    let mut wrong_purpose = fitting_input(fitting_events(&SurfaceTrainingPlan::v1()));
    wrong_purpose.surface_plan = Some(held_out_binding.clone());
    assert!(matches!(
        CalibrationCaptureJournal::new(wrong_purpose),
        Err(CalibrationCaptureError::SurfacePlanPurposeMismatch)
    ));

    assert!(matches!(
        held_out_binding.validate_locked_artifact([0x5c; 32]),
        Err(CalibrationCaptureError::SurfacePlanArtifactMismatch)
    ));

    let wrong_artifact = HeldOutSurfacePlan::v1([0x5c; 32], HELD_OUT_SEED)
        .canonical_json()
        .expect("wrong-artifact plan serializes");
    assert!(matches!(
        held_out_binding.validate_plan_json(&wrong_artifact),
        Err(CalibrationCaptureError::SurfacePlan(
            SurfacePlanError::ArtifactBindingMismatch
        ))
    ));

    let wrong_seed = HeldOutSurfacePlan::v1(ARTIFACT_SHA256, HELD_OUT_SEED + 1)
        .canonical_json()
        .expect("wrong-seed plan serializes");
    assert!(matches!(
        held_out_binding.validate_plan_json(&wrong_seed),
        Err(CalibrationCaptureError::SurfacePlan(
            SurfacePlanError::RunSeedBindingMismatch
        ))
    ));
}

#[test]
fn persisted_plan_hash_cannot_be_rebound_after_capture() {
    let journal = held_out_journal();
    let mut value: serde_json::Value =
        serde_json::from_slice(&journal.canonical_json().expect("journal serializes"))
            .expect("canonical journal is JSON");
    value["surface_plan"]["plan_sha256"] = serde_json::json!(format_sha256(&[0x44; 32]));

    assert!(matches!(
        CalibrationCaptureJournal::from_json(
            &serde_json::to_vec(&value).expect("tampered journal serializes")
        ),
        Err(CalibrationCaptureError::SurfacePlanIdentityMismatch)
    ));
}

#[test]
fn held_out_capture_requires_the_frozen_matching_rule() {
    let plan = HeldOutSurfacePlan::v1(ARTIFACT_SHA256, HELD_OUT_SEED);
    let binding = CalibrationSurfacePlanBinding::held_out(&plan, SURFACE_PLAN_STARTED_NS)
        .expect("plan binds");
    let mut input = capture_input(
        EvidencePurpose::HeldOutValidation,
        binding,
        held_out_events(&plan),
    );
    input.matching_rule = None;
    assert!(matches!(
        CalibrationCaptureJournal::new(input),
        Err(CalibrationCaptureError::MissingMatchingRule)
    ));
}

fn fitting_journal() -> CalibrationCaptureJournal {
    let plan = SurfaceTrainingPlan::v1();
    CalibrationCaptureJournal::new(fitting_input(fitting_events(&plan)))
        .expect("canonical fitting journal is valid")
}

fn held_out_journal() -> CalibrationCaptureJournal {
    let plan = HeldOutSurfacePlan::v1(ARTIFACT_SHA256, HELD_OUT_SEED);
    let binding = CalibrationSurfacePlanBinding::held_out(&plan, SURFACE_PLAN_STARTED_NS)
        .expect("plan binds");
    CalibrationCaptureJournal::new(capture_input(
        EvidencePurpose::HeldOutValidation,
        binding,
        held_out_events(&plan),
    ))
    .expect("canonical held-out journal is valid")
}

fn fitting_input(events: Vec<CapturedCalibrationEvent>) -> CalibrationCaptureInput {
    capture_input(
        EvidencePurpose::Fitting,
        CalibrationSurfacePlanBinding::fitting(SURFACE_PLAN_STARTED_NS).expect("plan binds"),
        events,
    )
}

fn capture_input(
    purpose: EvidencePurpose,
    surface_plan: CalibrationSurfacePlanBinding,
    events: Vec<CapturedCalibrationEvent>,
) -> CalibrationCaptureInput {
    CalibrationCaptureInput {
        purpose,
        acquisition_manifest_sha256: [0x11; 32],
        clock_epochs_sha256: [0x22; 32],
        session_sha256: [0x33; 32],
        session_id: SESSION_ID,
        keyboard_device_id: DEVICE_ID.to_owned(),
        keyboard: keyboard_identity(),
        setup_seconds: 30,
        created_unix_ns: 1,
        pre_rejected_examples: 0,
        neutral_windows: if purpose == EvidencePurpose::Fitting {
            vec![NeutralRecordWindow {
                block_id: "neutral-fixture".to_owned(),
                window_id: "still".to_owned(),
                role: NeutralWindowRole::Neutral,
                start_record: 0,
                end_record_exclusive: 32,
            }]
        } else {
            Vec::new()
        },
        matching_rule: (purpose == EvidencePurpose::HeldOutValidation).then(matching_rule),
        surface_plan: Some(surface_plan),
        events,
    }
}

fn fitting_events(plan: &SurfaceTrainingPlan) -> Vec<CapturedCalibrationEvent> {
    scheduled_surface_events(plan.segments())
}

fn held_out_events(plan: &HeldOutSurfacePlan) -> Vec<CapturedCalibrationEvent> {
    scheduled_surface_events(plan.segments())
}

fn scheduled_surface_events(segments: &[SurfacePromptSegment]) -> Vec<CapturedCalibrationEvent> {
    let mut events = Vec::new();
    let mut cumulative_render_delay_ns = 0_u64;
    let mut instruction_revision = 1_u64;
    for segment in segments {
        for scheduled in segment.scheduled_presentations() {
            let planned_host_time_ns = SURFACE_PLAN_STARTED_NS
                .checked_add(scheduled.at_ns())
                .and_then(|time| time.checked_add(cumulative_render_delay_ns))
                .expect("fixture surface schedule fits the host clock");
            // Keep a real positive browser delay so the fixture proves every
            // later display and null fact includes cumulative schedule shift.
            let render_delay_ns = 2_000_000 + instruction_revision % 3;
            let published_host_time_ns = planned_host_time_ns + render_delay_ns / 2;
            let acknowledged_host_time_ns = planned_host_time_ns + render_delay_ns;
            events.push(CapturedCalibrationEvent::DisplayAcknowledged {
                prompt_id: segment.id().clone(),
                presentation: SurfacePresentationReceipt::new(
                    planned_host_time_ns,
                    published_host_time_ns,
                    acknowledged_host_time_ns,
                    instruction_revision,
                )
                .expect("fixture presentation is ordered"),
            });
            cumulative_render_delay_ns += render_delay_ns;
            instruction_revision += 1;
        }
        let actual_segment_start_ns =
            SURFACE_PLAN_STARTED_NS + segment.start_ns() + cumulative_render_delay_ns;
        let actual_segment_end_ns =
            SURFACE_PLAN_STARTED_NS + segment.end_ns() + cumulative_render_delay_ns;
        match segment.kind() {
            SurfaceSegmentKind::QuietNull => {
                for sample_ordinal in 0..2 {
                    let trigger_host_time_ns =
                        actual_segment_start_ns + 100_000_000 + u64::from(sample_ordinal);
                    assert!(trigger_host_time_ns < actual_segment_end_ns);
                    events.push(CapturedCalibrationEvent::Idle {
                        prompt_id: segment.id().clone(),
                        sample_ordinal,
                        trigger_host_time_ns,
                    });
                }
            }
            SurfaceSegmentKind::ThresholdQuiet | SurfaceSegmentKind::OrdinaryMotion => {
                let trigger_host_time_ns = actual_segment_start_ns + 100_000_000;
                assert!(trigger_host_time_ns < actual_segment_end_ns);
                events.push(CapturedCalibrationEvent::Idle {
                    prompt_id: segment.id().clone(),
                    sample_ordinal: 0,
                    trigger_host_time_ns,
                });
            }
            SurfaceSegmentKind::PromptedTap | SurfaceSegmentKind::ThresholdTap => {}
        }
    }
    events
}

fn display_indices(events: &[CapturedCalibrationEvent]) -> Vec<usize> {
    events
        .iter()
        .enumerate()
        .filter_map(|(index, event)| {
            matches!(event, CapturedCalibrationEvent::DisplayAcknowledged { .. }).then_some(index)
        })
        .collect()
}

fn display_index_for_prompt(events: &[CapturedCalibrationEvent], prompt_id: &str) -> usize {
    events
        .iter()
        .position(|event| {
            matches!(
                event,
                CapturedCalibrationEvent::DisplayAcknowledged {
                    prompt_id: actual,
                    ..
                } if actual.as_str() == prompt_id
            )
        })
        .expect("fixture contains the requested display acknowledgement")
}

fn presentation_at(
    events: &[CapturedCalibrationEvent],
    index: usize,
) -> SurfacePresentationReceipt {
    match events[index] {
        CapturedCalibrationEvent::DisplayAcknowledged { presentation, .. } => presentation,
        _ => panic!("fixture index is a display acknowledgement"),
    }
}

fn replace_presentation(
    events: &mut [CapturedCalibrationEvent],
    index: usize,
    replacement: SurfacePresentationReceipt,
) {
    let CapturedCalibrationEvent::DisplayAcknowledged { presentation, .. } = &mut events[index]
    else {
        panic!("fixture index is a display acknowledgement");
    };
    *presentation = replacement;
}

fn keyboard_identity() -> KeyboardIdentity {
    KeyboardIdentity {
        name: "Fixture Keyboard".to_owned(),
        physical_path: "fixture/input0".to_owned(),
        vendor_id: 0x1234,
        product_id: 0x5678,
        logical_layout: "us-qwerty-v1".to_owned(),
        layout_confirmation: KeyboardLayoutConfirmationReceipt::us_qwerty(1, 1),
    }
}

fn matching_rule() -> CalibrationMatchingRule {
    CalibrationMatchingRule {
        schema: CALIBRATION_MATCHING_RULE_SCHEMA.to_owned(),
        acquisition_manifest_sha256: format_sha256(&[0x11; 32]),
        neutral_windows_sha256: format_sha256(&[0x22; 32]),
        neutral_source_sha256: format_sha256(&[0x33; 32]),
        feature_schema: CONTACT_FEATURE_SCHEMA.to_owned(),
        thresholds: [
            HandContactThresholds {
                hand: Hand::Left,
                low_energy: 1_024,
                high_energy: 4_096,
                motion_energy: 4_096,
            },
            HandContactThresholds {
                hand: Hand::Right,
                low_energy: 1_024,
                high_energy: 4_096,
                motion_energy: 4_096,
            },
        ],
        debounce_windows: CONTACT_DEBOUNCE_WINDOWS,
        rearm_windows: CONTACT_REARM_WINDOWS,
        response_start_ns: CONTACT_RESPONSE_START_NS,
        response_end_ns: CONTACT_RESPONSE_END_NS,
    }
}

#[cfg(target_os = "linux")]
mod keyboard_completion {
    use super::*;
    use invisible_keyboard::{
        calibration::KEYBOARD_EXAMPLES_PER_POSITION,
        calibration_worker::{
            CalibrationCaptureWorker, CalibrationCaptureWorkerConfig, KeyboardCaptureContext,
        },
        decoder::TapWindow,
        device_transport::{DeviceTransportConfig, DeviceTransportEvent},
        labels::{KeyboardSourceIdentity, LEFT_SHIFT_PHYSICAL_CODE},
        layout::{PhysicalPosition, position_index},
        manifest::{
            ACQUISITION_MANIFEST_SCHEMA, AcquisitionManifest, CORE28_LAYOUT_ID, HandManifest,
            ManifestHand, PROTOCOL_VERSION, SensorManifest, SensorRole,
        },
        prompts::PromptCorpus,
        simulation::SyntheticConfig,
        transport::encode_transport_frame,
    };

    #[test]
    fn complete_press_waits_for_release_and_keeps_original_down_provenance() {
        let mut capture = Capture::new();
        let a = position_for_usage(KeyUsage::A).expect("A has a physical position");
        let down_time = capture.tap_window(a);
        capture.key(30, Some(a), RawKeyState::Down, down_time, 10, false);
        capture.assert_coverage(a, 0);

        // A state-only release in a later context completes the original press.
        // Its timestamp cannot match the tap; only the preserved down can.
        capture.key(
            30,
            Some(a),
            RawKeyState::Up,
            down_time + 500_000_000,
            99,
            true,
        );
        capture.assert_coverage(a, 1);
        let bundle = capture.worker.checkpoint().expect("complete press matches");
        assert_eq!(bundle.evidence.observations().len(), 1);
        let observation = &bundle.evidence.observations()[0];
        assert_eq!(observation.trigger_host_time_ns(), down_time);
        assert_eq!(observation.role(), EvidenceRole::Training);
        assert_eq!(observation.domain(), EvidenceDomain::Keyboard);
        assert_eq!(observation.block_id(), 10);
        assert!(bundle.evidence.key_state_neutral());
        assert_eq!(bundle.capture_report.raw_keyboard_events, 4);
        assert_eq!(bundle.capture_report.accepted_key_downs, 1);
        assert_eq!(bundle.capture_report.rejected_keyboard_events, 0);
        assert_eq!(bundle.capture_report.unmatched_observations, 0);
        assert_eq!(bundle.journal.events.len(), 4);
        let CapturedCalibrationEvent::Keyboard { event, .. } = &bundle.journal.events[2] else {
            panic!("original down remains a raw keyboard fact");
        };
        assert_eq!(event.host_time_ns, down_time);
        assert_eq!(event.kernel_time_ns, Some(down_time));
    }

    #[test]
    fn overlapping_presses_remain_raw_without_matched_observations_or_coverage() {
        for state_only_overlap in [false, true] {
            let mut capture = Capture::new();
            let a = position_for_usage(KeyUsage::A).expect("A has a physical position");
            let s = position_for_usage(KeyUsage::S).expect("S has a physical position");
            let down_time = capture.tap_window(a);
            capture.key(30, Some(a), RawKeyState::Down, down_time, 10, false);
            capture.key(
                31,
                Some(s),
                RawKeyState::Down,
                down_time + 1,
                11,
                state_only_overlap,
            );
            capture.key(30, Some(a), RawKeyState::Up, down_time + 2, 99, true);
            capture.key(
                31,
                Some(s),
                RawKeyState::Up,
                down_time + 3,
                11,
                state_only_overlap,
            );
            capture.assert_coverage(a, 0);
            capture.assert_coverage(s, 0);

            let clean_time = capture.clean_tap(a);
            let bundle = capture
                .worker
                .checkpoint()
                .expect("later clean press matches");
            assert_eq!(bundle.evidence.observations().len(), 1);
            assert_eq!(
                bundle.evidence.observations()[0].trigger_host_time_ns(),
                clean_time
            );
            assert!(bundle.evidence.key_state_neutral());
            assert_eq!(bundle.capture_report.raw_keyboard_events, 8);
            assert_eq!(bundle.capture_report.accepted_key_downs, 1);
            assert_eq!(bundle.capture_report.rejected_keyboard_events, 4);
            assert_eq!(bundle.capture_report.unmatched_observations, 0);
            assert_eq!(
                bundle.journal.events.len(),
                8,
                "all rejected press facts remain raw"
            );
        }
    }

    #[test]
    fn modifier_after_down_invalidates_the_complete_press_including_state_only_events() {
        for state_only_modifier in [false, true] {
            let mut capture = Capture::new();
            let a = position_for_usage(KeyUsage::A).expect("A has a physical position");
            let down_time = capture.tap_window(a);
            capture.key(30, Some(a), RawKeyState::Down, down_time, 10, false);
            capture.key(
                LEFT_SHIFT_PHYSICAL_CODE,
                None,
                RawKeyState::Down,
                down_time + 1,
                11,
                state_only_modifier,
            );
            capture.key(30, Some(a), RawKeyState::Up, down_time + 2, 99, true);
            capture.key(
                LEFT_SHIFT_PHYSICAL_CODE,
                None,
                RawKeyState::Up,
                down_time + 3,
                11,
                state_only_modifier,
            );
            capture.assert_coverage(a, 0);

            let clean_time = capture.clean_tap(a);
            let bundle = capture
                .worker
                .checkpoint()
                .expect("later clean press matches");
            assert_eq!(bundle.evidence.observations().len(), 1);
            assert_eq!(
                bundle.evidence.observations()[0].trigger_host_time_ns(),
                clean_time
            );
            assert!(bundle.evidence.key_state_neutral());
            assert_eq!(bundle.capture_report.raw_keyboard_events, 8);
            assert_eq!(bundle.capture_report.accepted_key_downs, 1);
            assert_eq!(bundle.capture_report.rejected_keyboard_events, 3);
            assert_eq!(bundle.capture_report.unmatched_observations, 0);
            assert_eq!(
                bundle.journal.events.len(),
                8,
                "modifier and key facts remain raw"
            );
        }
    }

    #[test]
    fn unreleased_press_has_no_matched_observation_or_coverage_and_is_not_neutral() {
        let mut capture = Capture::new();
        let a = position_for_usage(KeyUsage::A).expect("A has a physical position");
        let s = position_for_usage(KeyUsage::S).expect("S has a physical position");
        let clean_time = capture.clean_tap(a);
        let pending_time = capture.tap_window(s);
        capture.key(31, Some(s), RawKeyState::Down, pending_time, 11, false);
        capture.assert_coverage(a, 1);
        capture.assert_coverage(s, 0);
        let bundle = capture
            .worker
            .checkpoint()
            .expect("clean evidence survives a pending press");
        assert_eq!(bundle.evidence.observations().len(), 1);
        assert_eq!(
            bundle.evidence.observations()[0].trigger_host_time_ns(),
            clean_time
        );
        assert!(!bundle.evidence.key_state_neutral());
        assert_eq!(bundle.capture_report.raw_keyboard_events, 5);
        assert_eq!(bundle.capture_report.accepted_key_downs, 1);
        assert_eq!(bundle.capture_report.rejected_keyboard_events, 1);
        assert_eq!(bundle.capture_report.unmatched_observations, 0);
        assert!(matches!(
            bundle.journal.events.last(),
            Some(CapturedCalibrationEvent::Keyboard { event, .. })
                if event.state == RawKeyState::Down && event.host_time_ns == pending_time
        ));
    }

    struct Capture {
        worker: CalibrationCaptureWorker,
        keyboard: KeyboardSourceIdentity,
        configs: [SyntheticConfig; 2],
        trial: u64,
    }

    impl Capture {
        fn new() -> Self {
            let manifest = capture_manifest();
            let keyboard = KeyboardSourceIdentity {
                device_id: DEVICE_ID.to_owned(),
                name: "Capture completion keyboard".to_owned(),
                physical_path: "fixture/input0".to_owned(),
                vendor_id: 0x1234,
                product_id: 0x5678,
            };
            let configs = std::array::from_fn(|index| {
                let hand = [Hand::Left, Hand::Right][index];
                let identity = manifest.hands[index]
                    .frame_identity()
                    .expect("wire identity");
                SyntheticConfig::new(hand, 0x6700 + index as u64)
                    .with_noise(0)
                    .with_shared_motion(0)
                    .with_wire_identity(identity.controller_id_sha256, identity.boot_id)
            });
            let worker = CalibrationCaptureWorker::new(CalibrationCaptureWorkerConfig {
                manifest,
                keyboard_source: keyboard.clone(),
                purpose: EvidencePurpose::Fitting,
                matching_rule: None,
                session_id: SESSION_ID,
                created_unix_ns: 1_786_233_600_000_000_000,
                started_host_time_ns: 1,
                surface_plan: None,
                transport: DeviceTransportConfig {
                    merge_lateness_ns: 10,
                    source_silence_timeout_ns: u64::MAX / 2,
                    max_pending_candidates: 16_384,
                },
            })
            .expect("real capture worker starts");
            let mut capture = Self {
                worker,
                keyboard,
                configs,
                trial: 0,
            };
            for _ in 0..3 {
                let windows = capture.configs.each_ref().map(|config| {
                    let mut frames = config.idle_window(capture.trial).frames().to_vec();
                    for (sample_index, frame) in frames.iter_mut().enumerate() {
                        let variation =
                            i16::try_from(sample_index % 3).expect("small variation") - 1;
                        for (sensor_index, sensor) in frame.sensors.iter_mut().enumerate() {
                            sensor.acceleration = [0, 0, 16_000 + variation];
                            sensor.angular_velocity = [0, 0, 0];
                            sensor.sample_time_offset_us =
                                i16::try_from(sensor_index).expect("small sensor index") * 20;
                        }
                    }
                    TapWindow::new(frames).expect("valid neutral window")
                });
                capture.push_pair(&windows);
            }
            capture
                .worker
                .set_neutral_windows(vec![NeutralRecordWindow {
                    block_id: "neutral-completion".to_owned(),
                    window_id: "stable-home-pose".to_owned(),
                    role: NeutralWindowRole::Neutral,
                    start_record: 0,
                    end_record_exclusive: capture.worker.record_count(),
                }])
                .expect("stable neutral prefix is retained");
            for (state, time) in [(RawKeyState::Down, 1), (RawKeyState::Up, 2)] {
                capture.key(LEFT_SHIFT_PHYSICAL_CODE, None, state, time, 10, true);
            }
            capture
                .worker
                .confirm_keyboard_layout(KeyboardLayoutConfirmationReceipt::us_qwerty(1, 2))
                .expect("selected keyboard layout is confirmed");
            capture
        }

        fn push_pair(&mut self, windows: &[TapWindow; 2]) -> [u64; 2] {
            let mut last = [0; 2];
            for index in 0..windows[0].frames().len() {
                for hand in [Hand::Left, Hand::Right] {
                    let frame = windows[hand as usize].frames()[index];
                    let time = 1_000_000_000 + frame.device_time_us * 1_000;
                    last[hand as usize] = time;
                    self.worker
                        .handle_sensor(DeviceTransportEvent::Bytes {
                            source: hand,
                            receipt_time_ns: time,
                            bytes: encode_transport_frame(&frame).expect("real frame encodes"),
                        })
                        .expect("real sensor frame is retained");
                }
            }
            self.worker
                .handle_sensor(DeviceTransportEvent::Tick {
                    host_time_ns: last.into_iter().max().expect("pair has times") + 11,
                })
                .expect("merge watermark releases both streams");
            self.trial += 1;
            last
        }

        fn tap_window(&mut self, position: PhysicalPosition) -> u64 {
            let windows = std::array::from_fn(|index| {
                if index == position.hand as usize {
                    self.configs[index].tap_window(position, self.trial)
                } else {
                    self.configs[index].idle_window(self.trial)
                }
            });
            self.push_pair(&windows)[position.hand as usize]
        }

        fn key(
            &mut self,
            physical_code: u16,
            position: Option<PhysicalPosition>,
            state: RawKeyState,
            host_time_ns: u64,
            block_id: u32,
            state_only: bool,
        ) {
            let context = KeyboardCaptureContext::new(
                EvidenceRole::Training,
                EvidenceDomain::Keyboard,
                block_id,
            );
            self.worker
                .handle_keyboard(
                    &self.keyboard,
                    RawKeyboardEvent {
                        device_id: self.keyboard.device_id.clone(),
                        physical_code,
                        position,
                        modifier: physical_code == LEFT_SHIFT_PHYSICAL_CODE,
                        state,
                        kernel_time_ns: Some(host_time_ns),
                        host_time_ns,
                    },
                    if state_only {
                        context.state_only()
                    } else {
                        context
                    },
                )
                .expect("raw keyboard fact is retained");
        }

        fn clean_tap(&mut self, position: PhysicalPosition) -> u64 {
            let time = self.tap_window(position);
            let physical_code = if position == position_for_usage(KeyUsage::A).expect("A position")
            {
                30
            } else {
                31
            };
            self.key(
                physical_code,
                Some(position),
                RawKeyState::Down,
                time,
                10,
                false,
            );
            self.key(
                physical_code,
                Some(position),
                RawKeyState::Up,
                time + 1,
                10,
                false,
            );
            time
        }

        fn assert_coverage(&self, position: PhysicalPosition, expected: u32) {
            let checkpoint = self
                .worker
                .keyboard_evidence_checkpoint()
                .expect("real canonical matcher derives coverage");
            let actual = checkpoint.position(position).expect("position exists");
            assert_eq!(actual.matched_examples, expected);
            assert_eq!(
                actual.matched_blocks,
                if expected == 0 { vec![] } else { vec![10] }
            );
            assert_eq!(
                checkpoint.deficits(KEYBOARD_EXAMPLES_PER_POSITION)
                    [position_index(position).expect("position index")],
                2 * u16::from(KEYBOARD_EXAMPLES_PER_POSITION)
                    - u16::try_from(expected).expect("small matched count"),
            );
        }
    }

    fn capture_manifest() -> AcquisitionManifest {
        AcquisitionManifest {
            schema: ACQUISITION_MANIFEST_SCHEMA.to_owned(),
            layout_id: CORE28_LAYOUT_ID.to_owned(),
            protocol_version: PROTOCOL_VERSION,
            hands: [ManifestHand::Left, ManifestHand::Right]
                .into_iter()
                .enumerate()
                .map(|(index, hand)| HandManifest {
                    hand,
                    controller_id: format!("completion-controller-{index}"),
                    boot_id: format!("{:02x}", index + 1).repeat(16),
                    sensors: std::array::from_fn(|sensor_index| SensorManifest {
                        sensor_index: u8::try_from(sensor_index).expect("small sensor index"),
                        mux_channel: u8::try_from(sensor_index).expect("small sensor index"),
                        role: [
                            SensorRole::Thumb,
                            SensorRole::Index,
                            SensorRole::Middle,
                            SensorRole::Ring,
                            SensorRole::Pinky,
                            SensorRole::Dorsal,
                        ][sensor_index],
                        mount_id: format!("strap-{index}-{sensor_index}"),
                        wiring_id: format!("wire-{index}-{sensor_index}"),
                        interrupt_pin: format!("gpio-{index}-{sensor_index}"),
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
            study_id: "keyboard-completion-contract".to_owned(),
            split_id: "fitting".to_owned(),
            participant_alias: "fixture".to_owned(),
            consent_record: "fixture-consent".to_owned(),
            retention_policy: "local-test-only".to_owned(),
            rig_evidence_sha256: format_sha256(&[0xbb; 32]),
            axis_characterization_sha256: format_sha256(&[0xcc; 32]),
            calibration_artifact_sha256: None,
            calibration_validation_sha256: None,
        }
    }
}
