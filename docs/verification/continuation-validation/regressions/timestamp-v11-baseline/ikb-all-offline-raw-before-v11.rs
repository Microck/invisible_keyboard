use std::{
    collections::{BTreeMap, BTreeSet},
    sync::LazyLock,
};

use invisible_keyboard::{
    artifact::{CalibrationArtifact, KeyboardIdentity, KeyboardLayoutConfirmationReceipt},
    calibration_app::{
        CalibrationApp, CaptureCalibrationInput, EvidenceMode, run_capture_calibration,
    },
    calibration_capture::{
        CalibrationCaptureError, CalibrationCaptureInput, CalibrationCaptureJournal,
        CalibrationMatchingRule, CalibrationSurfacePlanBinding, CapturedCalibrationEvent,
        SurfacePresentationReceipt, build_calibration_evidence, derive_matching_rule,
    },
    calibration_engine::{
        CalibrationEngineError, CalibrationEvidence, CalibrationFitInput, CalibrationFitResult,
        EvidenceDomain, EvidencePurpose, EvidenceRole, evaluate_keyboard_precheck, fit_calibration,
        validate_calibration,
    },
    clock_epoch::ClockEpochSet,
    labels::{LEFT_SHIFT_PHYSICAL_CODE, RawKeyState, RawKeyboardEvent},
    layout::{Hand, PhysicalPosition, all_slots, keyboard_warm_start_eligible},
    manifest::{
        ACQUISITION_MANIFEST_SCHEMA, AcquisitionManifest, CORE28_LAYOUT_ID, HandManifest,
        ManifestHand, PROTOCOL_VERSION, SensorManifest, SensorRole, format_sha256,
    },
    neutral::{NeutralEvidenceReport, NeutralRecordWindow, NeutralWindowRole},
    prompts::{
        HeldOutSurfacePlan, PromptCorpus, SurfacePromptSegment, SurfaceSegmentKind,
        SurfaceTargetMetadata, SurfaceTrainingPlan,
    },
    protocol::{ALL_SENSORS_VALID, SENSOR_COUNT, SensorFrame, SensorReading},
    session::{
        ControllerStream, FinalizedSession, SessionHeader, SessionRecordKind, SessionWriter,
    },
    simulation::SyntheticConfig,
    validation::HeldOutValidationError,
};

const FIT_SESSION_ID: [u8; 16] = [0x41; 16];
const VALIDATION_SESSION_ID: [u8; 16] = [0x42; 16];
const HELDOUT_RUN_SEED: u64 = 0x4845_4c44_4f55_5456;
const IDENTITY_AXIS_MAP: [i8; 9] = [1, 0, 0, 0, 1, 0, 0, 0, 1];
const ROTATED_AXIS_MAP: [i8; 9] = [0, 1, 0, -1, 0, 0, 0, 0, 1];
const PROMPT_SEPARATION_IDLE_WINDOWS: usize = 17;

#[derive(Clone)]
struct EvidenceFixture {
    session: FinalizedSession,
    clocks: ClockEpochSet,
    journal: CalibrationCaptureJournal,
    evidence: CalibrationEvidence,
    matching_rule: CalibrationMatchingRule,
    neutral_report: Option<NeutralEvidenceReport>,
}

fn manifest() -> AcquisitionManifest {
    AcquisitionManifest {
        schema: ACQUISITION_MANIFEST_SCHEMA.to_owned(),
        layout_id: CORE28_LAYOUT_ID.to_owned(),
        protocol_version: PROTOCOL_VERSION,
        hands: vec![
            hand_manifest(ManifestHand::Left, 0x11),
            hand_manifest(ManifestHand::Right, 0x22),
        ],
        controller_board: "Seeed XIAO nRF52840 Plus SKU 102010672".to_owned(),
        firmware_build: "fixture-firmware-digest-a1b2c3d4".to_owned(),
        host_build: "fixture-host-digest-e5f60718".to_owned(),
        sensor_model: "LSM6DS3TR-C".to_owned(),
        sensors_per_hand: 6,
        sensor_odr_hz: 208,
        accelerometer_range_g: 4,
        gyroscope_range_dps: 1_000,
        filter_profile: "gate0-raw-v1".to_owned(),
        i2c_bus_hz: 400_000,
        nominal_period_us: 5_000,
        prompt_corpus_sha256: format_sha256(&PromptCorpus::v1().sha256()),
        study_id: "guided-current-mount-calibration".to_owned(),
        split_id: "locked-before-held-out-validation".to_owned(),
        participant_alias: "fixture-operator".to_owned(),
        consent_record: "fixture-consent".to_owned(),
        retention_policy: "local-delete-after-analysis-v1".to_owned(),
        rig_evidence_sha256: format_sha256(&[0xbb; 32]),
        axis_characterization_sha256: format_sha256(&[0xcc; 32]),
        calibration_artifact_sha256: None,
        calibration_validation_sha256: None,
    }
}

fn rotated_axis_manifest() -> AcquisitionManifest {
    let mut manifest = manifest();
    for hand in &mut manifest.hands {
        for sensor in &mut hand.sensors {
            sensor.axis_map = ROTATED_AXIS_MAP;
        }
    }
    manifest
        .validate()
        .expect("the fixture uses a proper signed-permutation rotation");
    manifest
}

/// Convert one canonical anatomical vector back to the raw sensor axes that
/// produce it. A proper signed-permutation matrix is orthogonal, so its
/// inverse is its transpose and the integer conversion is exact.
fn encode_wire_vector(axis_map: [i8; 9], canonical: [i16; 3]) -> [i16; 3] {
    std::array::from_fn(|raw_axis| {
        let value = (0..3)
            .map(|canonical_axis| {
                i32::from(axis_map[canonical_axis * 3 + raw_axis])
                    * i32::from(canonical[canonical_axis])
            })
            .sum::<i32>();
        i16::try_from(value).expect("fixture signed permutation remains in the i16 range")
    })
}

fn encode_wire_frame(axis_maps: [[i8; 9]; SENSOR_COUNT], frame: &mut SensorFrame) {
    if axis_maps
        .iter()
        .all(|axis_map| *axis_map == IDENTITY_AXIS_MAP)
    {
        return;
    }
    for (sensor, axis_map) in frame.sensors.iter_mut().zip(axis_maps) {
        sensor.acceleration = encode_wire_vector(axis_map, sensor.acceleration);
        sensor.angular_velocity = encode_wire_vector(axis_map, sensor.angular_velocity);
    }
}

fn decoded_wire_frames(session: &FinalizedSession) -> Vec<SensorFrame> {
    let mut reader = session.reader();
    let mut frames = Vec::new();
    while let Some(record) = reader
        .next_record()
        .expect("fixture session remains readable")
    {
        if record.kind == SessionRecordKind::Decoded {
            frames.push(
                record
                    .decode()
                    .expect("fixture decoded record remains valid"),
            );
        }
    }
    frames
}

fn hand_manifest(hand: ManifestHand, boot: u8) -> HandManifest {
    let side = match hand {
        ManifestHand::Left => "left",
        ManifestHand::Right => "right",
    };
    HandManifest {
        hand,
        controller_id: format!("xiao-{side}-001"),
        boot_id: format!("{boot:02x}").repeat(16),
        sensors: std::array::from_fn(|sensor_index| SensorManifest {
            sensor_index: u8::try_from(sensor_index).expect("sensor index fits u8"),
            mux_channel: u8::try_from(sensor_index).expect("mux channel fits u8"),
            role: [
                SensorRole::Thumb,
                SensorRole::Index,
                SensorRole::Middle,
                SensorRole::Ring,
                SensorRole::Pinky,
                SensorRole::Dorsal,
            ][sensor_index],
            mount_id: format!("strap-{side}-{sensor_index}"),
            wiring_id: format!("wiring-{side}-{sensor_index}"),
            interrupt_pin: format!("gpio-{side}-{sensor_index}"),
            axis_map: IDENTITY_AXIS_MAP,
        }),
    }
}

fn keyboard() -> KeyboardIdentity {
    KeyboardIdentity {
        name: "Fixture USB Keyboard".to_owned(),
        physical_path: "fixture/input0".to_owned(),
        vendor_id: 0x1234,
        product_id: 0x5678,
        logical_layout: "us-qwerty-v1".to_owned(),
        layout_confirmation: KeyboardLayoutConfirmationReceipt::us_qwerty(1, 1),
    }
}

fn synthetic_config(manifest: &AcquisitionManifest, hand: Hand, seed: u64) -> SyntheticConfig {
    let identity = manifest.hands[hand as usize]
        .frame_identity()
        .expect("fixture manifest has wire identity");
    SyntheticConfig::new(hand, seed)
        // Independent synthetic sensor noise can cross the deliberately low
        // software-only contact floor before a prompted tap. Keep this
        // canonical matcher fixture deterministic; noise behavior has its own
        // detector contracts.
        .with_noise(0)
        .with_wire_identity(identity.controller_id_sha256, identity.boot_id)
}

fn fitting_fixture() -> EvidenceFixture {
    static FIXTURE: LazyLock<EvidenceFixture> =
        LazyLock::new(|| fitting_fixture_with_wire_axis(&manifest(), None));
    // Share only construction of canonical input. Each test gets isolated
    // journals and diagnostics and still runs its own engine operation.
    FIXTURE.clone()
}

fn fitting_fixture_with_wire_axis(
    manifest: &AcquisitionManifest,
    wire_axis_map: Option<[i8; 9]>,
) -> EvidenceFixture {
    let mut builder =
        SessionFixtureBuilder::new(manifest, FIT_SESSION_ID).with_wire_axis_map(wire_axis_map);
    builder.identify_keyboard();
    for hand in [Hand::Left, Hand::Right] {
        let config = synthetic_config(manifest, hand, 0x1000 + hand as u64);
        for slot in all_slots().iter().filter(|slot| {
            slot.position.hand == hand && keyboard_warm_start_eligible(slot.position)
        }) {
            for example in 0..8 {
                builder.push(
                    &config,
                    Some(slot.position),
                    EvidenceRole::Training,
                    EvidenceDomain::Keyboard,
                    if example < 4 { 10 } else { 11 },
                );
            }
        }
    }
    builder.append_fitting_surface_plan();
    builder
        .finish(EvidencePurpose::Fitting, 300, None)
        .expect("canonical fitting capture derives evidence")
}

fn validation_fixture(
    manifest: &AcquisitionManifest,
    artifact: &CalibrationArtifact,
    matching_rule: &CalibrationMatchingRule,
) -> EvidenceFixture {
    validation_fixture_with_windows(manifest, artifact, matching_rule, 12, None, |_| {})
}

fn validation_fixture_with_full_exposure(
    manifest: &AcquisitionManifest,
    artifact: &CalibrationArtifact,
    matching_rule: &CalibrationMatchingRule,
) -> EvidenceFixture {
    validation_fixture_with_windows(manifest, artifact, matching_rule, 800, None, |_| {})
}

fn validation_fixture_with_wire_axis(
    manifest: &AcquisitionManifest,
    artifact: &CalibrationArtifact,
    matching_rule: &CalibrationMatchingRule,
    wire_axis_map: [i8; 9],
) -> EvidenceFixture {
    validation_fixture_with_windows(
        manifest,
        artifact,
        matching_rule,
        12,
        Some(wire_axis_map),
        |_| {},
    )
}

fn validation_fixture_with_extra(
    manifest: &AcquisitionManifest,
    artifact: &CalibrationArtifact,
    matching_rule: &CalibrationMatchingRule,
    add_extra_stream: impl FnOnce(&mut SessionFixtureBuilder<'_>),
) -> EvidenceFixture {
    validation_fixture_with_windows(
        manifest,
        artifact,
        matching_rule,
        12,
        None,
        add_extra_stream,
    )
}

fn validation_fixture_with_windows(
    manifest: &AcquisitionManifest,
    artifact: &CalibrationArtifact,
    matching_rule: &CalibrationMatchingRule,
    ordinary_motion_windows_per_hand: usize,
    wire_axis_map: Option<[i8; 9]>,
    add_extra_stream: impl FnOnce(&mut SessionFixtureBuilder<'_>),
) -> EvidenceFixture {
    let mut builder = SessionFixtureBuilder::new(manifest, VALIDATION_SESSION_ID)
        .with_wire_axis_map(wire_axis_map);
    builder.append_held_out_surface_plan(artifact, ordinary_motion_windows_per_hand, true);
    add_extra_stream(&mut builder);
    builder
        .finish(EvidencePurpose::HeldOutValidation, 390, Some(matching_rule))
        .expect("canonical held-out capture derives evidence")
}

struct SessionFixtureBuilder<'a> {
    manifest: &'a AcquisitionManifest,
    session_id: [u8; 16],
    writer: SessionWriter<Vec<u8>>,
    events: Vec<CapturedCalibrationEvent>,
    surface_plan: Option<CalibrationSurfacePlanBinding>,
    trials: [u64; 2],
    record_ordinal: u64,
    next_instruction_revision: u64,
    wire_axis_maps: [[[i8; 9]; SENSOR_COUNT]; 2],
}

impl<'a> SessionFixtureBuilder<'a> {
    fn new(manifest: &'a AcquisitionManifest, session_id: [u8; 16]) -> Self {
        let writer = SessionWriter::new(
            Vec::new(),
            SessionHeader {
                nominal_period_us: manifest.nominal_period_us,
                manifest_sha256: manifest.sha256().expect("manifest hashes"),
                session_id,
            },
        )
        .expect("session starts");
        let wire_axis_maps = std::array::from_fn(|hand_index| {
            manifest.hands[hand_index]
                .sensors
                .each_ref()
                .map(|sensor| sensor.axis_map)
        });
        Self {
            manifest,
            session_id,
            writer,
            events: Vec::new(),
            surface_plan: None,
            trials: [0, 0],
            record_ordinal: 0,
            next_instruction_revision: 1,
            wire_axis_maps,
        }
    }

    fn with_wire_axis_map(mut self, wire_axis_map: Option<[i8; 9]>) -> Self {
        if let Some(wire_axis_map) = wire_axis_map {
            self.wire_axis_maps = [[wire_axis_map; SENSOR_COUNT]; 2];
        }
        self
    }

    fn identify_keyboard(&mut self) {
        for (state, host_time_ns) in [(RawKeyState::Down, 1_u64), (RawKeyState::Up, 2_u64)] {
            self.events
                .push(CapturedCalibrationEvent::KeyboardStateOnly {
                    role: EvidenceRole::Training,
                    domain: EvidenceDomain::Keyboard,
                    block_id: 0,
                    event: RawKeyboardEvent {
                        device_id: "fixture-keyboard".to_owned(),
                        physical_code: LEFT_SHIFT_PHYSICAL_CODE,
                        position: None,
                        modifier: true,
                        state,
                        kernel_time_ns: None,
                        host_time_ns,
                    },
                });
        }
    }

    fn push(
        &mut self,
        config: &SyntheticConfig,
        position: Option<PhysicalPosition>,
        role: EvidenceRole,
        domain: EvidenceDomain,
        block_id: u32,
    ) {
        assert_eq!(domain, EvidenceDomain::Keyboard);
        let position = position.expect("keyboard fixture events have a physical position");
        let (_, _, trigger_host_time_ns) = self.append_window(config, Some(position));
        for (state, host_time_ns) in [
            (RawKeyState::Down, trigger_host_time_ns),
            (RawKeyState::Up, trigger_host_time_ns.saturating_add(1)),
        ] {
            self.events.push(CapturedCalibrationEvent::Keyboard {
                role,
                domain,
                block_id,
                event: RawKeyboardEvent {
                    device_id: "fixture-keyboard".to_owned(),
                    physical_code: 30,
                    position: Some(position),
                    modifier: false,
                    state,
                    kernel_time_ns: None,
                    host_time_ns,
                },
            });
        }
    }

    fn push_unlabeled(&mut self, config: &SyntheticConfig, position: Option<PhysicalPosition>) {
        self.append_window(config, position);
    }

    fn append_window(
        &mut self,
        config: &SyntheticConfig,
        position: Option<PhysicalPosition>,
    ) -> (Hand, u64, u64) {
        let hand = position.map_or(config.hand(), |position| position.hand);
        let hand_index = hand as usize;
        let trial = self.trials[hand_index];
        self.trials[hand_index] += 1;
        let window = position.map_or_else(
            || config.idle_window(trial),
            |position| config.tap_window(position, trial),
        );
        let mut trigger_host_time_ns = 0;
        for frame in window.frames() {
            let mut frame = *frame;
            // Both controller clocks advance on the one fixture-wide timeline.
            // Per-hand sequence numbers remain source-local, while device-time
            // gaps honestly reflect samples captured from the other hand.
            frame.device_time_us = self
                .record_ordinal
                .saturating_mul(u64::from(self.manifest.nominal_period_us));
            // SyntheticConfig produces canonical anatomical vectors. Persist
            // the inverse-rotated sensor-native values so production fitting
            // and validation must apply the manifest axis map themselves.
            encode_wire_frame(self.wire_axis_maps[hand_index], &mut frame);
            trigger_host_time_ns = 1_000_000_000 + frame.device_time_us * 1_000;
            self.writer
                .append_frame(ControllerStream::from(hand), &frame, trigger_host_time_ns)
                .expect("fixture frame writes");
            self.record_ordinal += 1;
        }
        (hand, self.record_ordinal - 1, trigger_host_time_ns)
    }

    fn append_fitting_surface_plan(&mut self) {
        let plan = SurfaceTrainingPlan::v1();
        let started_host_time_ns = self.next_frame_host_time_ns().saturating_add(2_000_000_000);
        self.append_surface_plan(
            plan.segments(),
            plan.scoring_targets(),
            0,
            false,
            started_host_time_ns,
            CalibrationSurfacePlanBinding::fitting(started_host_time_ns)
                .expect("canonical fitting plan binds"),
        );
    }

    fn append_held_out_surface_plan(
        &mut self,
        artifact: &CalibrationArtifact,
        ordinary_motion_windows_per_hand: usize,
        measured_motion: bool,
    ) {
        let plan = HeldOutSurfacePlan::v1(
            artifact.fingerprint().expect("fixture artifact hashes"),
            HELDOUT_RUN_SEED,
        );
        let started_host_time_ns = self.next_frame_host_time_ns().saturating_add(2_000_000_000);
        let binding = CalibrationSurfacePlanBinding::held_out(&plan, started_host_time_ns)
            .expect("canonical held-out plan binds");
        self.append_surface_plan(
            plan.segments(),
            plan.scoring_targets(),
            ordinary_motion_windows_per_hand,
            measured_motion,
            started_host_time_ns,
            binding,
        );
    }

    fn append_surface_plan(
        &mut self,
        segments: &[SurfacePromptSegment],
        targets: &[SurfaceTargetMetadata],
        ordinary_motion_windows_per_hand: usize,
        measured_motion: bool,
        started_host_time_ns: u64,
        binding: CalibrationSurfacePlanBinding,
    ) {
        assert!(
            self.surface_plan.is_none(),
            "one fixture has one surface plan"
        );
        let target_positions = targets
            .iter()
            .map(|target| (target.prompt_id().as_str(), target.position()))
            .collect::<BTreeMap<_, _>>();
        let ordinary_segment_counts: [usize; 2] = std::array::from_fn(|hand_index| {
            let hand = [Hand::Left, Hand::Right][hand_index];
            segments
                .iter()
                .filter(|segment| {
                    segment.kind() == SurfaceSegmentKind::OrdinaryMotion
                        && segment.motion_hand() == Some(hand)
                })
                .count()
        });
        let mut ordinary_segments_seen = [0_usize; 2];
        let mut ordinary_windows_emitted = [0_usize; 2];

        for segment in segments {
            for scheduled in segment.scheduled_presentations() {
                let host_time_ns = started_host_time_ns
                    .checked_add(scheduled.at_ns())
                    .expect("fixture surface schedule fits the host clock");
                self.events
                    .push(CapturedCalibrationEvent::DisplayAcknowledged {
                        prompt_id: segment.id().clone(),
                        presentation: SurfacePresentationReceipt::new(
                            host_time_ns,
                            host_time_ns,
                            host_time_ns,
                            self.next_instruction_revision,
                        )
                        .expect("fixture display acknowledgement is causal"),
                    });
                self.next_instruction_revision += 1;
            }
            match segment.kind() {
                SurfaceSegmentKind::PromptedTap | SurfaceSegmentKind::ThresholdTap => {
                    let position = target_positions
                        .get(segment.id().as_str())
                        .copied()
                        .expect("every canonical tap segment has scoring metadata");
                    let config = synthetic_config(
                        self.manifest,
                        position.hand,
                        0x3000 + position.hand as u64,
                    );
                    // Preserve the production response-window contract: the
                    // next prompted contact must fall after this prompt's
                    // 1.5-second association window. These continuous quiet
                    // frames also give the detector enough time to rearm.
                    for _ in 0..PROMPT_SEPARATION_IDLE_WINDOWS {
                        self.append_window(&config, None);
                    }
                    let acknowledged_host_time_ns = started_host_time_ns
                        .checked_add(
                            segment
                                .cue_at_ns()
                                .expect("tap segment owns a canonical cue"),
                        )
                        .expect("fixture cue fits the host clock");
                    self.pad_idle_until_next_window_end(
                        &config,
                        acknowledged_host_time_ns + 200_000_000,
                    );
                    assert!(
                        self.next_window_end_host_time_ns()
                            <= acknowledged_host_time_ns + 1_500_000_000,
                        "fixture contact stays inside the canonical response interval"
                    );
                    let (_, _, trigger_host_time_ns) = self.append_window(&config, Some(position));
                    assert!(trigger_host_time_ns >= acknowledged_host_time_ns);
                }
                SurfaceSegmentKind::QuietNull => {
                    for (sample_ordinal, hand) in [Hand::Left, Hand::Right].into_iter().enumerate()
                    {
                        self.append_idle_fact(
                            segment,
                            hand,
                            sample_ordinal as u32,
                            false,
                            started_host_time_ns,
                        );
                    }
                }
                SurfaceSegmentKind::ThresholdQuiet => {
                    self.append_idle_fact(
                        segment,
                        segment
                            .motion_hand()
                            .expect("threshold quiet owns one hand"),
                        0,
                        false,
                        started_host_time_ns,
                    );
                }
                SurfaceSegmentKind::OrdinaryMotion => {
                    let hand = segment.motion_hand().expect("motion segment owns one hand");
                    let hand_index = hand as usize;
                    let segments_left = ordinary_segment_counts[hand_index]
                        .saturating_sub(ordinary_segments_seen[hand_index]);
                    let windows_left = ordinary_motion_windows_per_hand
                        .saturating_sub(ordinary_windows_emitted[hand_index]);
                    let segment_windows = windows_left / segments_left;
                    for sample_ordinal in 0..segment_windows {
                        self.append_idle_fact(
                            segment,
                            hand,
                            u32::try_from(sample_ordinal)
                                .expect("fixture motion sample count fits u32"),
                            measured_motion,
                            started_host_time_ns,
                        );
                    }
                    ordinary_segments_seen[hand_index] += 1;
                    ordinary_windows_emitted[hand_index] += segment_windows;
                }
            }
        }
        assert_eq!(
            ordinary_windows_emitted,
            [ordinary_motion_windows_per_hand; 2]
        );
        self.surface_plan = Some(binding);
    }

    fn append_idle_fact(
        &mut self,
        segment: &SurfacePromptSegment,
        hand: Hand,
        sample_ordinal: u32,
        measured_motion: bool,
        started_host_time_ns: u64,
    ) {
        let earliest_host_time_ns = started_host_time_ns
            .checked_add(segment.start_ns())
            .and_then(|time| time.checked_add(100_000_000))
            .expect("fixture null schedule fits the host clock");
        let latest_host_time_ns = started_host_time_ns
            .checked_add(segment.end_ns())
            .expect("fixture null schedule fits the host clock");
        let idle_config = synthetic_config(self.manifest, hand, 0x4000 + hand as u64);
        self.pad_idle_until_next_window_end(&idle_config, earliest_host_time_ns);
        let config = synthetic_config(self.manifest, hand, 0x4000 + hand as u64)
            .with_shared_motion(if measured_motion { 2_000 } else { 0 });
        let (_, _, trigger_host_time_ns) = self.append_window(&config, None);
        assert!(
            trigger_host_time_ns >= earliest_host_time_ns
                && trigger_host_time_ns < latest_host_time_ns,
            "fixture null truth window stays inside its canonical segment"
        );
        self.events.push(CapturedCalibrationEvent::Idle {
            prompt_id: segment.id().clone(),
            sample_ordinal,
            trigger_host_time_ns,
        });
    }

    fn pad_idle_until_next_window_end(
        &mut self,
        config: &SyntheticConfig,
        earliest_host_time_ns: u64,
    ) {
        while self.next_window_end_host_time_ns() < earliest_host_time_ns {
            self.append_window(config, None);
        }
    }

    fn next_frame_host_time_ns(&self) -> u64 {
        1_000_000_000 + self.record_ordinal * u64::from(self.manifest.nominal_period_us) * 1_000
    }

    fn next_window_end_host_time_ns(&self) -> u64 {
        let window_end_record = self.record_ordinal
            + u64::try_from(invisible_keyboard::pipeline::WINDOW_FRAMES - 1)
                .expect("window length fits u64");
        1_000_000_000 + window_end_record * u64::from(self.manifest.nominal_period_us) * 1_000
    }

    fn finish(
        mut self,
        purpose: EvidencePurpose,
        setup_seconds: u32,
        inherited_matching_rule: Option<&CalibrationMatchingRule>,
    ) -> Result<EvidenceFixture, CalibrationCaptureError> {
        let neutral_windows = if purpose == EvidencePurpose::Fitting {
            vec![self.append_neutral_block()]
        } else {
            Vec::new()
        };
        let session = self
            .writer
            .finish_finalized()
            .expect("session finalizes and audits");
        let clocks =
            ClockEpochSet::fit_session(self.manifest, &session).expect("fixture clocks fit");
        let matching_rule = inherited_matching_rule.cloned().unwrap_or_else(|| {
            derive_matching_rule(self.manifest, &session, &neutral_windows)
                .expect("neutral sources derive a contact rule")
        });
        matching_rule.validate().expect("matching rule is valid");
        let neutral_report = if purpose == EvidencePurpose::Fitting {
            Some(
                NeutralEvidenceReport::derive(self.manifest, &clocks, &session, &neutral_windows)
                    .expect("fixture neutral evidence is stable"),
            )
        } else {
            None
        };
        let journal = CalibrationCaptureJournal::new(CalibrationCaptureInput {
            purpose,
            acquisition_manifest_sha256: self.manifest.sha256().expect("manifest hashes"),
            clock_epochs_sha256: clocks.fingerprint().expect("clocks hash"),
            session_sha256: session.sha256(),
            session_id: self.session_id,
            keyboard_device_id: "fixture-keyboard".to_owned(),
            keyboard: keyboard(),
            setup_seconds,
            created_unix_ns: 1_786_233_600_000_000_000,
            pre_rejected_examples: 2,
            neutral_windows,
            matching_rule: Some(matching_rule.clone()),
            surface_plan: self.surface_plan,
            events: self.events,
        })?;
        journal.validate_complete_surface_plan()?;
        let (evidence, capture_report) =
            build_calibration_evidence(self.manifest, &clocks, &journal, &session)?;
        assert_eq!(capture_report.matching_rule, matching_rule);
        Ok(EvidenceFixture {
            session,
            clocks,
            journal,
            evidence,
            matching_rule,
            neutral_report,
        })
    }

    fn append_neutral_block(&mut self) -> NeutralRecordWindow {
        let start_record = self.record_ordinal;
        for hand in [Hand::Left, Hand::Right] {
            let hand_index = hand as usize;
            let trial = self.trials[hand_index];
            let sequence_base = u32::try_from(trial)
                .expect("fixture trial fits u32")
                .saturating_mul(16);
            let identity = self.manifest.hands[hand_index]
                .frame_identity()
                .expect("fixture manifest has wire identity");
            for sample_index in 0..40_u32 {
                let small_variation =
                    i16::try_from(sample_index % 3).expect("small remainder fits i16") - 1;
                let sensors = std::array::from_fn(|sensor_index| SensorReading {
                    acceleration: [0, 0, 16_000 + small_variation],
                    angular_velocity: [0, 0, 0],
                    sample_time_offset_us: i16::try_from(sensor_index)
                        .expect("sensor index fits i16")
                        * 20,
                });
                let mut frame = SensorFrame {
                    hand,
                    controller_id_sha256: identity.controller_id_sha256,
                    boot_id: identity.boot_id,
                    flags: 0,
                    valid_sensor_mask: ALL_SENSORS_VALID,
                    sequence: sequence_base.saturating_add(sample_index),
                    device_time_us: self
                        .record_ordinal
                        .saturating_mul(u64::from(self.manifest.nominal_period_us)),
                    sensors,
                };
                encode_wire_frame(self.wire_axis_maps[hand_index], &mut frame);
                let host_time_ns = 1_000_000_000 + frame.device_time_us * 1_000;
                self.writer
                    .append_frame(ControllerStream::from(hand), &frame, host_time_ns)
                    .expect("neutral fixture frame writes");
                self.record_ordinal = self.record_ordinal.saturating_add(1);
            }
        }
        NeutralRecordWindow {
            block_id: "fixture-neutral".to_owned(),
            window_id: "stable-two-hand-hold".to_owned(),
            role: NeutralWindowRole::Neutral,
            start_record,
            end_record_exclusive: self.record_ordinal,
        }
    }
}

fn fit_from_fixture(
    manifest: &AcquisitionManifest,
    fixture: &EvidenceFixture,
) -> Result<CalibrationFitResult, CalibrationEngineError> {
    fit_calibration(CalibrationFitInput {
        manifest,
        clocks: &fixture.clocks,
        journal: &fixture.journal,
        session: &fixture.session,
        neutral_report: fixture
            .neutral_report
            .as_ref()
            .expect("fitting fixture has neutral evidence"),
    })
}

fn append_state_only_keyboard_pair(
    journal: &mut CalibrationCaptureJournal,
    role: EvidenceRole,
    down_host_time_ns: u64,
) {
    let position = all_slots()[0].position;
    for (state, host_time_ns) in [
        (RawKeyState::Down, down_host_time_ns),
        (RawKeyState::Up, down_host_time_ns.saturating_add(1)),
    ] {
        journal
            .events
            .push(CapturedCalibrationEvent::KeyboardStateOnly {
                role,
                domain: EvidenceDomain::Keyboard,
                block_id: 99,
                event: RawKeyboardEvent {
                    device_id: "fixture-keyboard".to_owned(),
                    physical_code: 30,
                    position: Some(position),
                    modifier: false,
                    state,
                    kernel_time_ns: None,
                    host_time_ns,
                },
            });
    }
}

fn first_passive_tap_host_time(fixture: &EvidenceFixture) -> u64 {
    fixture
        .evidence
        .observations()
        .iter()
        .find(|observation| {
            observation.domain() == EvidenceDomain::PassiveSurface
                && observation.position().is_some()
        })
        .expect("fixture contains passive tap truth")
        .trigger_host_time_ns()
}

fn record_host_time(session: &FinalizedSession, target_record: u64) -> u64 {
    let mut reader = session.reader();
    let mut record_ordinal = 0_u64;
    while let Some(record) = reader.next_record().expect("fixture session reads") {
        if record_ordinal == target_record {
            return record.host_time_ns;
        }
        record_ordinal += 1;
    }
    panic!("fixture record {target_record} exists");
}

fn alter_offline_clock(
    clocks: &ClockEpochSet,
    field: &str,
    value: serde_json::Value,
) -> ClockEpochSet {
    let mut json = serde_json::to_value(clocks).expect("clock serializes");
    json["epochs"][0]["fit"]["report"][field] = value;
    serde_json::from_value(json).expect("altered clock shape remains valid")
}

fn receipt_fault_session(session: &FinalizedSession, fault: u8) -> FinalizedSession {
    let mut writer = SessionWriter::new(Vec::new(), *session.header()).expect("session starts");
    let mut reader = session.reader();
    let mut ordinal = 0_u64;
    while let Some(record) = reader.next_record().expect("raw session reads") {
        let mut frame = record.decode().expect("fixture contains sensor records");
        if fault == 1 && ordinal == 40 {
            frame.flags |= invisible_keyboard::protocol::FLAG_CLOCK_UNSYNCED;
        }
        let phase = (ordinal / 2) % 32;
        let delay_ns = match fault {
            0 => phase.min(32 - phase) * 1_000_000,
            1 => 0,
            2 => record.host_time_ns / 100,
            _ => unreachable!("fixture has three fault modes"),
        };
        writer
            .append_frame(
                ControllerStream::from(frame.hand),
                &frame,
                record.host_time_ns + delay_ns,
            )
            .expect("raw receipt evidence writes");
        ordinal += 1;
    }
    writer.finish_finalized().expect("raw session finalizes")
}

#[test]
fn offline_authority_consumers_reject_forged_clock_statistics_and_fingerprints() {
    let manifest = manifest();
    let fitting = fitting_fixture();
    let fitted = fit_from_fixture(&manifest, &fitting).expect("canonical fitting succeeds");
    let heldout = validation_fixture(&manifest, &fitted.artifact, &fitting.matching_rule);
    for source in [&fitting, &heldout] {
        for fault in 0..3 {
            let session = receipt_fault_session(&source.session, fault);
            assert!(
                ClockEpochSet::fit_session(&manifest, &session).is_err(),
                "real receipts or controller clock fault fail independent qualification"
            );
            let mut journal = source.journal.clone();
            journal.session_sha256 = format_sha256(&session.sha256());
            journal.clock_epochs_sha256 = format_sha256(&source.clocks.fingerprint().unwrap());
            assert!(
                build_calibration_evidence(&manifest, &source.clocks, &journal, &session).is_err(),
                "matching raw hashes and claimed passing statistics cannot grant capture authority"
            );
            if source.journal.purpose == EvidencePurpose::Fitting {
                assert!(
                    evaluate_keyboard_precheck(&manifest, &source.clocks, &journal, &session)
                        .is_err()
                );
                assert!(
                    fit_calibration(CalibrationFitInput {
                        manifest: &manifest,
                        clocks: &source.clocks,
                        journal: &journal,
                        session: &session,
                        neutral_report: source.neutral_report.as_ref().unwrap(),
                    })
                    .is_err()
                );
            } else {
                assert!(
                    validate_calibration(
                        &manifest,
                        &source.clocks,
                        &fitted.artifact,
                        &journal,
                        &session
                    )
                    .is_err(),
                    "held-out authority cannot trust forged passing clock statistics"
                );
            }
        }
    }
}

#[test]
fn offline_authority_consumers_reject_altered_valid_looking_clock_reports() {
    let manifest = manifest();
    let fitting = fitting_fixture();
    let fitted = fit_from_fixture(&manifest, &fitting).expect("canonical fitting succeeds");
    let heldout = validation_fixture(&manifest, &fitted.artifact, &fitting.matching_rule);
    for source in [&fitting, &heldout] {
        for (field, value) in [
            ("slope_ns_per_us", serde_json::json!(1000.01)),
            ("drift_ppm", serde_json::json!(1.0)),
            ("residual_p95_ns", serde_json::json!(1)),
            (
                "sample_count",
                serde_json::json!(source.clocks.epochs[0].fit.report().sample_count + 1),
            ),
        ] {
            let clocks = alter_offline_clock(&source.clocks, field, value);
            clocks
                .validate()
                .expect("forged report still claims passing numeric gates");
            let mut journal = source.journal.clone();
            journal.clock_epochs_sha256 = format_sha256(&clocks.fingerprint().unwrap());
            assert!(
                build_calibration_evidence(&manifest, &clocks, &journal, &source.session).is_err(),
                "altered {field} cannot grant capture authority"
            );
            if source.journal.purpose == EvidencePurpose::Fitting {
                assert!(
                    evaluate_keyboard_precheck(&manifest, &clocks, &journal, &source.session)
                        .is_err()
                );
                assert!(
                    fit_calibration(CalibrationFitInput {
                        manifest: &manifest,
                        clocks: &clocks,
                        journal: &journal,
                        session: &source.session,
                        neutral_report: source.neutral_report.as_ref().unwrap(),
                    })
                    .is_err()
                );
            } else {
                assert!(
                    validate_calibration(
                        &manifest,
                        &clocks,
                        &fitted.artifact,
                        &journal,
                        &source.session
                    )
                    .is_err(),
                    "altered {field} cannot grant held-out authority"
                );
            }
        }
    }
}

#[test]
fn capture_bound_fit_locks_and_validates_two_hands() {
    let manifest = manifest();
    let fitting = fitting_fixture();
    let precheck = evaluate_keyboard_precheck(
        &manifest,
        &fitting.clocks,
        &fitting.journal,
        &fitting.session,
    )
    .expect("keyboard-labelled signal evidence passes the precheck");
    assert_eq!(precheck.metrics.intent_separation_basis_points, 10_000);
    assert_eq!(precheck.metrics.finger_agreement_basis_points, 10_000);
    assert_eq!(precheck.evaluated_windows, 29 * 8);
    assert!(precheck.weak_positions.is_empty());
    let preliminary_fit = fit_from_fixture(&manifest, &fitting)
        .expect("canonical fitting capture produces the locked plan identity");
    // Eight hundred non-overlapping 75 ms windows give each hand exactly 60
    // seconds of measured exposure for the integration path that mints admission.
    let held_out = validation_fixture_with_full_exposure(
        &manifest,
        &preliminary_fit.artifact,
        &fitting.matching_rule,
    );
    let app = CalibrationApp::new(EvidenceMode::RecordedReplay);
    let run = run_capture_calibration(
        &app,
        CaptureCalibrationInput {
            manifest: &manifest,
            fitting_clocks: &fitting.clocks,
            fitting_journal: &fitting.journal,
            fitting_session: &fitting.session,
            neutral_report: fitting
                .neutral_report
                .as_ref()
                .expect("fitting fixture has neutral evidence"),
            validation_clocks: &held_out.clocks,
            validation_journal: &held_out.journal,
            validation_session: &held_out.session,
        },
    )
    .expect("immutable fitting and held-out files drive the user state machine");
    assert!(run.snapshot.heldout_validation_passed);
    assert_eq!(run.snapshot.evidence, "recorded-replay");
    assert_eq!(
        run.snapshot.rejected_examples,
        fitting.evidence.rejected_examples()
    );
    assert!(run.artifact.decoder(Hand::Left).is_some());
    assert!(run.artifact.decoder(Hand::Right).is_some());
    assert_eq!(
        run.artifact
            .locked_identity()
            .expect("artifact identity is complete")
            .source_sessions()
            .len(),
        1
    );
    assert_eq!(run.validation_report.ordinary_motion_exposure_ms, 60_000);
    assert_eq!(run.validation_report.ordinary_motion_activations(), 0);

    let diagnostic_json = fitting
        .evidence
        .canonical_json()
        .expect("diagnostic evidence serializes");
    let diagnostic_value: serde_json::Value =
        serde_json::from_slice(&diagnostic_json).expect("diagnostic JSON is inspectable");
    assert_eq!(diagnostic_value["purpose"], "fitting");
}

#[test]
fn keyboard_warm_start_labels_cannot_change_passive_fitted_artifact() {
    let manifest = manifest();
    let fitting = fitting_fixture();
    let baseline = fit_from_fixture(&manifest, &fitting)
        .expect("baseline passive-only fitting succeeds")
        .artifact;
    let mut relabeled = fitting.journal.clone();

    // Rotate keyboard labels among positions owned by the same acting finger,
    // preserving the warm-start ownership check, exact coverage, and blocks.
    // If keyboard examples entered the passive position or threshold fit,
    // this adversarial row relabeling would change the decoder artifact.
    for hand in [Hand::Left, Hand::Right] {
        for event in &mut relabeled.events {
            let CapturedCalibrationEvent::Keyboard {
                role: EvidenceRole::Training,
                domain: EvidenceDomain::Keyboard,
                event,
                ..
            } = event
            else {
                continue;
            };
            let Some(position) = event.position.filter(|position| position.hand == hand) else {
                continue;
            };
            let eligible = all_slots()
                .iter()
                .map(|slot| slot.position)
                .filter(|candidate| {
                    candidate.hand == hand
                        && candidate.finger == position.finger
                        && keyboard_warm_start_eligible(*candidate)
                })
                .collect::<Vec<_>>();
            let index = eligible
                .iter()
                .position(|candidate| *candidate == position)
                .expect("keyboard fixture uses an eligible position");
            event.position = Some(eligible[(index + 1) % eligible.len()]);
        }
    }

    let adversarial = fit_calibration(CalibrationFitInput {
        manifest: &manifest,
        clocks: &fitting.clocks,
        journal: &relabeled,
        session: &fitting.session,
        neutral_report: fitting
            .neutral_report
            .as_ref()
            .expect("fitting fixture has neutral evidence"),
    })
    .expect("relabeled keyboard evidence still satisfies warm-start coverage")
    .artifact;
    for hand in [Hand::Left, Hand::Right] {
        assert_eq!(
            serde_json::to_vec(baseline.decoder(hand).expect("baseline decoder exists"))
                .expect("baseline decoder serializes"),
            serde_json::to_vec(
                adversarial
                    .decoder(hand)
                    .expect("adversarial decoder exists")
            )
            .expect("adversarial decoder serializes"),
            "keyboard labels must not enter passive centroids or intent threshold"
        );
    }
    assert_ne!(
        baseline.fitting_capture_journal_sha256, adversarial.fitting_capture_journal_sha256,
        "artifact provenance still identifies the exact fitting journal"
    );
}

#[test]
fn down_up_overlap_cannot_be_reused_as_passive_training_truth() {
    let manifest = manifest();
    let fitting = fitting_fixture();

    let mut nonoverlap = fitting.journal.clone();
    append_state_only_keyboard_pair(&mut nonoverlap, EvidenceRole::Training, u64::MAX - 1);
    fit_calibration(CalibrationFitInput {
        manifest: &manifest,
        clocks: &fitting.clocks,
        journal: &nonoverlap,
        session: &fitting.session,
        neutral_report: fitting
            .neutral_report
            .as_ref()
            .expect("fitting fixture has neutral evidence"),
    })
    .expect("keyboard activity outside passive source windows remains diagnostic");

    let mut overlapping = fitting.journal.clone();
    append_state_only_keyboard_pair(
        &mut overlapping,
        EvidenceRole::Training,
        first_passive_tap_host_time(&fitting),
    );
    assert!(matches!(
        fit_calibration(CalibrationFitInput {
            manifest: &manifest,
            clocks: &fitting.clocks,
            journal: &overlapping,
            session: &fitting.session,
            neutral_report: fitting
                .neutral_report
                .as_ref()
                .expect("fitting fixture has neutral evidence"),
        }),
        Err(CalibrationEngineError::Capture(
            CalibrationCaptureError::NoSurfaceContact { .. }
        ))
    ));
}

#[test]
fn down_up_overlap_cannot_tune_neutral_contact_thresholds() {
    let manifest = manifest();
    let fitting = fitting_fixture();
    let neutral = fitting
        .journal
        .neutral_windows
        .first()
        .expect("fitting fixture has neutral truth");
    let mut overlapping = fitting.journal.clone();
    append_state_only_keyboard_pair(
        &mut overlapping,
        EvidenceRole::Training,
        record_host_time(&fitting.session, neutral.start_record),
    );

    assert!(matches!(
        fit_calibration(CalibrationFitInput {
            manifest: &manifest,
            clocks: &fitting.clocks,
            journal: &overlapping,
            session: &fitting.session,
            neutral_report: fitting
                .neutral_report
                .as_ref()
                .expect("fitting fixture has neutral evidence"),
        }),
        Err(CalibrationEngineError::Capture(
            CalibrationCaptureError::KeyboardActivityOverlapsPassiveEvidence { .. }
        ))
    ));
}

#[test]
fn down_up_overlap_cannot_count_as_held_out_truth() {
    let manifest = manifest();
    let fitting = fitting_fixture();
    let artifact = fit_from_fixture(&manifest, &fitting)
        .expect("fitting succeeds")
        .artifact;
    let held_out = validation_fixture(&manifest, &artifact, &fitting.matching_rule);

    let mut nonoverlap = held_out.journal.clone();
    append_state_only_keyboard_pair(
        &mut nonoverlap,
        EvidenceRole::HeldOutValidation,
        u64::MAX - 1,
    );
    validate_calibration(
        &manifest,
        &held_out.clocks,
        &artifact,
        &nonoverlap,
        &held_out.session,
    )
    .expect("keyboard activity outside held-out source windows remains diagnostic");

    let mut overlapping = held_out.journal.clone();
    append_state_only_keyboard_pair(
        &mut overlapping,
        EvidenceRole::HeldOutValidation,
        first_passive_tap_host_time(&held_out),
    );
    assert!(matches!(
        validate_calibration(
            &manifest,
            &held_out.clocks,
            &artifact,
            &overlapping,
            &held_out.session,
        ),
        Err(CalibrationEngineError::Capture(
            CalibrationCaptureError::NoSurfaceContact { .. }
        ))
    ));
}

#[test]
fn held_out_validation_cannot_mutate_the_locked_artifact() {
    let manifest = manifest();
    let fitting = fitting_fixture();
    let artifact = fit_from_fixture(&manifest, &fitting)
        .expect("fitting succeeds")
        .artifact;
    let before = artifact
        .canonical_json()
        .expect("locked artifact serializes before held-out evaluation");
    let held_out = validation_fixture(&manifest, &artifact, &fitting.matching_rule);

    validate_calibration(
        &manifest,
        &held_out.clocks,
        &artifact,
        &held_out.journal,
        &held_out.session,
    )
    .expect("held-out validation succeeds without fitting or policy selection");

    assert_eq!(
        before,
        artifact
            .canonical_json()
            .expect("locked artifact serializes after held-out evaluation")
    );
}

#[test]
fn signed_permutation_wire_axes_are_equivalent_through_fit_and_held_out_validation() {
    let identity_manifest = manifest();
    let rotated_manifest = rotated_axis_manifest();
    let identity_fitting = fitting_fixture();
    let rotated_fitting = fitting_fixture_with_wire_axis(&rotated_manifest, None);

    // Prove that this is a raw-wire fixture, not two manifests pointed at the
    // same canonical bytes. Every raw rotated vector is the exact inverse-map
    // encoding of its identity-axis counterpart, and at least one differs.
    let identity_wire = decoded_wire_frames(&identity_fitting.session);
    let rotated_wire = decoded_wire_frames(&rotated_fitting.session);
    assert_eq!(identity_wire.len(), rotated_wire.len());
    let mut observed_non_identity_vector = false;
    for (identity_frame, rotated_frame) in identity_wire.iter().zip(&rotated_wire) {
        assert_eq!(identity_frame.hand, rotated_frame.hand);
        assert_eq!(identity_frame.sequence, rotated_frame.sequence);
        for (identity_sensor, rotated_sensor) in
            identity_frame.sensors.iter().zip(&rotated_frame.sensors)
        {
            assert_eq!(
                rotated_sensor.acceleration,
                encode_wire_vector(ROTATED_AXIS_MAP, identity_sensor.acceleration)
            );
            assert_eq!(
                rotated_sensor.angular_velocity,
                encode_wire_vector(ROTATED_AXIS_MAP, identity_sensor.angular_velocity)
            );
            observed_non_identity_vector |= identity_sensor.acceleration
                != rotated_sensor.acceleration
                || identity_sensor.angular_velocity != rotated_sensor.angular_velocity;
        }
    }
    assert!(
        observed_non_identity_vector,
        "the fixture must exercise a vector changed by the signed permutation"
    );

    let identity_precheck = evaluate_keyboard_precheck(
        &identity_manifest,
        &identity_fitting.clocks,
        &identity_fitting.journal,
        &identity_fitting.session,
    )
    .expect("identity-axis keyboard evidence passes the precheck");
    let rotated_precheck = evaluate_keyboard_precheck(
        &rotated_manifest,
        &rotated_fitting.clocks,
        &rotated_fitting.journal,
        &rotated_fitting.session,
    )
    .expect("rotated raw-wire keyboard evidence passes the same precheck");
    assert_eq!(identity_precheck, rotated_precheck);

    let identity_fit = fit_from_fixture(&identity_manifest, &identity_fitting)
        .expect("identity-axis fitting succeeds");
    let rotated_fit = fit_from_fixture(&rotated_manifest, &rotated_fitting)
        .expect("rotated raw-wire fitting succeeds");
    assert_eq!(identity_fit.accepted_windows, rotated_fit.accepted_windows);
    for hand in [Hand::Left, Hand::Right] {
        let identity_decoder = identity_fit
            .artifact
            .decoder(hand)
            .expect("identity artifact has both decoders");
        let rotated_decoder = rotated_fit
            .artifact
            .decoder(hand)
            .expect("rotated artifact has both decoders");
        assert_eq!(
            serde_json::to_vec(identity_decoder).expect("identity decoder serializes"),
            serde_json::to_vec(rotated_decoder).expect("rotated decoder serializes"),
            "fitted {hand:?} decoder changed after an equivalent wire-axis rotation"
        );
    }

    // Negative control for the fitting boundary: persist the same rotated
    // wire vectors but declare identity maps, which is exactly the effective
    // input if fitting omits normalization. At least one fitted decoder must
    // then change, so the equality assertion above cannot pass by accident.
    let unnormalized_fitting =
        fitting_fixture_with_wire_axis(&identity_manifest, Some(ROTATED_AXIS_MAP));
    let unnormalized_fit = fit_from_fixture(&identity_manifest, &unnormalized_fitting)
        .expect("the fitting negative control remains structurally valid");
    assert!([Hand::Left, Hand::Right].into_iter().any(|hand| {
        serde_json::to_vec(
            identity_fit
                .artifact
                .decoder(hand)
                .expect("identity artifact has both decoders"),
        )
        .expect("identity decoder serializes")
            != serde_json::to_vec(
                unnormalized_fit
                    .artifact
                    .decoder(hand)
                    .expect("negative-control artifact has both decoders"),
            )
            .expect("negative-control decoder serializes")
    }));

    // Negative control for the held-out boundary. These bundles have the
    // same declared identity manifest, labels, timing, and sample count. Only
    // the raw vectors differ, simulating held-out normalization being absent.
    let identity_short_held_out = validation_fixture(
        &identity_manifest,
        &identity_fit.artifact,
        &identity_fitting.matching_rule,
    );
    let unnormalized_short_held_out = validation_fixture_with_wire_axis(
        &identity_manifest,
        &identity_fit.artifact,
        &identity_fitting.matching_rule,
        ROTATED_AXIS_MAP,
    );
    let identity_short_validation = validate_calibration(
        &identity_manifest,
        &identity_short_held_out.clocks,
        &identity_fit.artifact,
        &identity_short_held_out.journal,
        &identity_short_held_out.session,
    )
    .expect("identity held-out negative-control baseline validates");
    let unnormalized_short_validation = validate_calibration(
        &identity_manifest,
        &unnormalized_short_held_out.clocks,
        &identity_fit.artifact,
        &unnormalized_short_held_out.journal,
        &unnormalized_short_held_out.session,
    )
    .expect("unnormalized held-out negative control remains structurally valid");
    assert_ne!(
        identity_short_validation.causal_predictions,
        unnormalized_short_validation.causal_predictions,
        "held-out omission control must change causal decoder output"
    );
    assert_ne!(
        identity_short_validation.report.confusion, unnormalized_short_validation.report.confusion,
        "held-out omission control must change scored confusion"
    );

    let identity_held_out = validation_fixture_with_full_exposure(
        &identity_manifest,
        &identity_fit.artifact,
        &identity_fitting.matching_rule,
    );
    let rotated_held_out = validation_fixture_with_full_exposure(
        &rotated_manifest,
        &rotated_fit.artifact,
        &rotated_fitting.matching_rule,
    );
    let identity_validation = validate_calibration(
        &identity_manifest,
        &identity_held_out.clocks,
        &identity_fit.artifact,
        &identity_held_out.journal,
        &identity_held_out.session,
    )
    .expect("identity-axis held-out validation succeeds");
    let rotated_validation = validate_calibration(
        &rotated_manifest,
        &rotated_held_out.clocks,
        &rotated_fit.artifact,
        &rotated_held_out.journal,
        &rotated_held_out.session,
    )
    .expect("rotated raw-wire held-out validation succeeds");

    // Each artifact fingerprint owns a distinct shuffled held-out schedule, so
    // prediction vector order is not comparable across the two artifacts.
    // The order-independent scored result must still be identical.
    assert_eq!(
        identity_validation.report.confusion, rotated_validation.report.confusion,
        "held-out confusion changed across equivalent raw axes"
    );
    assert_eq!(identity_validation.metrics, rotated_validation.metrics);
    assert_eq!(
        identity_validation.heldout_validation_passed(),
        rotated_validation.heldout_validation_passed()
    );
    assert!(identity_validation.heldout_validation_passed());
    let identity_admission = identity_validation
        .held_out_validation()
        .expect("the raw identity-axis held-out evaluation mints admission");
    let rotated_admission = rotated_validation
        .held_out_validation()
        .expect("the raw rotated-axis held-out evaluation mints admission");
    assert_eq!(identity_admission.metrics(), identity_validation.metrics);
    assert_eq!(rotated_admission.metrics(), rotated_validation.metrics);
    assert_eq!(
        identity_admission.artifact_sha256(),
        identity_fit
            .artifact
            .fingerprint()
            .expect("identity artifact hashes")
    );
    assert_eq!(
        identity_admission.validation_report_sha256(),
        identity_validation
            .report
            .fingerprint()
            .expect("identity validation report hashes")
    );
    assert_eq!(
        identity_validation.evaluated_windows,
        rotated_validation.evaluated_windows
    );
    assert_eq!(
        identity_validation.report.ordinary_motion_exposure_ms,
        rotated_validation.report.ordinary_motion_exposure_ms
    );
    assert_eq!(
        identity_validation.report.key_state_neutral,
        rotated_validation.report.key_state_neutral
    );
}

#[test]
fn immutable_identity_rejects_tampered_session_and_journal_binding() {
    let manifest = manifest();
    let fitting = fitting_fixture();
    let mut tampered = fitting.session.bytes().to_vec();
    let last = tampered.len() - 1;
    tampered[last] ^= 1;
    assert!(FinalizedSession::audit(tampered).is_err());

    let mut wrong_session = fitting.journal.clone();
    wrong_session.session_sha256 = format_sha256(&[0x99; 32]);
    assert!(matches!(
        fit_calibration(CalibrationFitInput {
            manifest: &manifest,
            clocks: &fitting.clocks,
            journal: &wrong_session,
            session: &fitting.session,
            neutral_report: fitting
                .neutral_report
                .as_ref()
                .expect("fitting fixture has neutral evidence"),
        }),
        Err(CalibrationEngineError::Capture(
            CalibrationCaptureError::SessionIdentityMismatch
        ))
    ));
}

#[test]
fn canonical_surface_plan_completeness_is_enforced_before_fit() {
    let manifest = manifest();
    let fitting = fitting_fixture();
    let mut incomplete = fitting.journal.clone();
    let remove = incomplete
        .events
        .iter()
        .position(|event| matches!(event, CapturedCalibrationEvent::DisplayAcknowledged { .. }))
        .expect("surface prompt fact exists");
    incomplete.events.remove(remove);

    assert!(matches!(
        fit_calibration(CalibrationFitInput {
            manifest: &manifest,
            clocks: &fitting.clocks,
            journal: &incomplete,
            session: &fitting.session,
            neutral_report: fitting
                .neutral_report
                .as_ref()
                .expect("fitting fixture has neutral evidence"),
        }),
        Err(CalibrationEngineError::Capture(
            CalibrationCaptureError::ReorderedSurfacePrompt { .. }
                | CalibrationCaptureError::MissingSurfacePrompt(_)
                | CalibrationCaptureError::SurfacePromptTimingMismatch(_)
        ))
    ));
}

#[test]
fn derived_truth_windows_are_record_disjoint_within_each_session() {
    fn assert_disjoint(fixture: &EvidenceFixture) {
        let mut hand_records: [Vec<u64>; 2] = std::array::from_fn(|_| Vec::new());
        let mut reader = fixture.session.reader();
        let mut record_ordinal = 0_u64;
        while let Some(record) = reader.next_record().expect("fixture session reads") {
            if record.kind == SessionRecordKind::Decoded {
                let frame = record.decode().expect("fixture sensor record decodes");
                hand_records[frame.hand as usize].push(record_ordinal);
            }
            record_ordinal += 1;
        }

        let mut reserved = BTreeSet::new();
        for observation in fixture.evidence.observations() {
            let records = &hand_records[observation.hand() as usize];
            let end_index = records
                .binary_search(&observation.window_end_record())
                .expect("derived window end belongs to its recorded hand");
            let start_index = end_index
                .checked_add(1)
                .and_then(|end_exclusive| {
                    end_exclusive.checked_sub(invisible_keyboard::pipeline::WINDOW_FRAMES)
                })
                .expect("derived evidence owns a complete causal window");
            for source_record in &records[start_index..=end_index] {
                assert!(
                    reserved.insert(*source_record),
                    "derived truth windows reuse source record {source_record}"
                );
            }
        }
    }

    let manifest = manifest();
    let fitting = fitting_fixture();
    let artifact = fit_from_fixture(&manifest, &fitting)
        .expect("fitting succeeds")
        .artifact;
    let held_out = validation_fixture(&manifest, &artifact, &fitting.matching_rule);

    // Each evidence/session bind owns its own reservation set. Prediction may
    // still read every held-out frame from the continuous stream.
    assert_disjoint(&fitting);
    assert_disjoint(&held_out);
}

#[test]
fn fitting_and_validation_reject_wrong_purpose_journals() {
    let manifest = manifest();
    let fitting = fitting_fixture();
    let artifact = fit_from_fixture(&manifest, &fitting)
        .expect("fitting succeeds")
        .artifact;
    let held_out = validation_fixture(&manifest, &artifact, &fitting.matching_rule);

    assert!(matches!(
        validate_calibration(
            &manifest,
            &fitting.clocks,
            &artifact,
            &fitting.journal,
            &fitting.session,
        ),
        Err(CalibrationEngineError::WrongPurpose)
    ));
    assert!(matches!(
        fit_calibration(CalibrationFitInput {
            manifest: &manifest,
            clocks: &held_out.clocks,
            journal: &held_out.journal,
            session: &held_out.session,
            neutral_report: fitting
                .neutral_report
                .as_ref()
                .expect("fitting fixture has neutral evidence"),
        }),
        Err(CalibrationEngineError::WrongPurpose)
    ));
}

#[test]
fn held_out_keyboard_identity_must_match_the_locked_artifact() {
    let manifest = manifest();
    let fitting = fitting_fixture();
    let artifact = fit_from_fixture(&manifest, &fitting)
        .expect("fitting succeeds")
        .artifact;
    let held_out = validation_fixture(&manifest, &artifact, &fitting.matching_rule);
    let mut wrong_keyboard = held_out.journal.clone();
    wrong_keyboard.keyboard.product_id ^= 1;

    assert!(matches!(
        validate_calibration(
            &manifest,
            &held_out.clocks,
            &artifact,
            &wrong_keyboard,
            &held_out.session,
        ),
        Err(CalibrationEngineError::KeyboardIdentityMismatch)
    ));
}

#[test]
fn unlabeled_impulses_are_counted_as_spontaneous_false_activations() {
    let manifest = manifest();
    let fitting = fitting_fixture();
    let artifact = fit_from_fixture(&manifest, &fitting)
        .expect("fitting succeeds")
        .artifact;
    let held_out =
        validation_fixture_with_extra(&manifest, &artifact, &fitting.matching_rule, |builder| {
            let config = synthetic_config(&manifest, Hand::Left, 0x2000 + Hand::Left as u64);
            let position = all_slots()
                .iter()
                .find(|slot| slot.label == "A")
                .expect("A exists")
                .position;
            for _ in 0..PROMPT_SEPARATION_IDLE_WINDOWS {
                builder.push_unlabeled(&config, None);
            }
            builder.push_unlabeled(&config, Some(position));
            builder.push_unlabeled(&config, None);
        });

    let validated = validate_calibration(
        &manifest,
        &held_out.clocks,
        &artifact,
        &held_out.journal,
        &held_out.session,
    )
    .expect("continuous held-out evaluation succeeds");

    assert_eq!(validated.report.ordinary_motion_activations(), 1);
    assert!(!validated.heldout_validation_passed());
}

#[test]
fn recorded_exposure_is_derived_from_matched_motion_windows() {
    let manifest = manifest();
    let fitting = fitting_fixture();
    let artifact = fit_from_fixture(&manifest, &fitting)
        .expect("fitting succeeds")
        .artifact;
    let held_out = validation_fixture(&manifest, &artifact, &fitting.matching_rule);

    let validated = validate_calibration(
        &manifest,
        &held_out.clocks,
        &artifact,
        &held_out.journal,
        &held_out.session,
    )
    .expect("matched motion windows control recorded exposure");

    assert_eq!(validated.report.ordinary_motion_exposure_ms, 900);
    assert!(matches!(
        validated.held_out_validation(),
        Err(HeldOutValidationError::ThresholdsFailed {
            ordinary_motion_exposure_ms: 900,
            ..
        })
    ));
}

#[test]
fn held_out_validation_rederives_evidence_from_plan_bound_capture_journal() {
    let manifest = manifest();
    let fitting = fitting_fixture();
    let artifact = fit_from_fixture(&manifest, &fitting)
        .expect("fitting succeeds")
        .artifact;
    let held_out = validation_fixture(&manifest, &artifact, &fitting.matching_rule);

    let validated = validate_calibration(
        &manifest,
        &held_out.clocks,
        &artifact,
        &held_out.journal,
        &held_out.session,
    )
    .expect("held-out validation rederives diagnostics from raw inputs");

    assert!(validated.report.key_state_neutral);
    assert_eq!(
        validated
            .diagnostic_evidence
            .canonical_json()
            .expect("rederived diagnostics serialize"),
        held_out
            .evidence
            .canonical_json()
            .expect("fixture diagnostics serialize")
    );
}

#[test]
fn diagnostic_evidence_json_cannot_enter_trusted_engine_apis() {
    let manifest = manifest();
    let fitting = fitting_fixture();
    let artifact = fit_from_fixture(&manifest, &fitting)
        .expect("fitting succeeds")
        .artifact;
    let held_out = validation_fixture(&manifest, &artifact, &fitting.matching_rule);
    let baseline = validate_calibration(
        &manifest,
        &held_out.clocks,
        &artifact,
        &held_out.journal,
        &held_out.session,
    )
    .expect("baseline held-out evaluation succeeds");
    let mut edited_diagnostic: serde_json::Value = serde_json::from_slice(
        &held_out
            .evidence
            .canonical_json()
            .expect("diagnostic evidence serializes"),
    )
    .expect("diagnostic evidence is JSON");
    edited_diagnostic["ordinary_motion_exposure_ms"] = serde_json::json!(123_456);
    edited_diagnostic["key_state_neutral"] = serde_json::json!(false);

    // There is deliberately no trusted API operand for `edited_diagnostic`.
    // Re-evaluation accepts only the plan-bound journal and raw session.
    let rederived = validate_calibration(
        &manifest,
        &held_out.clocks,
        &artifact,
        &held_out.journal,
        &held_out.session,
    )
    .expect("diagnostic edits cannot affect canonical rederivation");

    assert_eq!(baseline.report, rederived.report);
    assert_eq!(baseline.causal_predictions, rederived.causal_predictions);
}

#[test]
fn held_out_plan_rejects_cross_artifact_reuse() {
    let manifest = manifest();
    let fitting = fitting_fixture();
    let artifact = fit_from_fixture(&manifest, &fitting)
        .expect("fitting succeeds")
        .artifact;
    let held_out = validation_fixture(&manifest, &artifact, &fitting.matching_rule);
    let mut different_artifact = artifact.clone();
    different_artifact.fitting_capture_journal_sha256 = format_sha256(&[0x91; 32]);

    assert!(matches!(
        validate_calibration(
            &manifest,
            &held_out.clocks,
            &different_artifact,
            &held_out.journal,
            &held_out.session,
        ),
        Err(CalibrationEngineError::Capture(
            CalibrationCaptureError::SurfacePlanArtifactMismatch
        ))
    ));
}

#[test]
fn fitting_session_overlap_is_derived_from_artifact_sources() {
    let manifest = manifest();
    let fitting = fitting_fixture();
    let artifact = fit_from_fixture(&manifest, &fitting)
        .expect("fitting succeeds")
        .artifact;
    let held_out = validation_fixture(&manifest, &artifact, &fitting.matching_rule);

    // The raw session identity is checked against fitting provenance before
    // any evidence can be rederived from a mismatched held-out journal.
    assert!(matches!(
        validate_calibration(
            &manifest,
            &fitting.clocks,
            &artifact,
            &held_out.journal,
            &fitting.session,
        ),
        Err(CalibrationEngineError::ValidationEvidence(
            invisible_keyboard::validation::ValidationEvidenceError::TrainingSessionOverlap
        ))
    ));
}

#[test]
fn quiet_plan_windows_cannot_satisfy_ordinary_motion() {
    let manifest = manifest();
    let fitting = fitting_fixture();
    let artifact = fit_from_fixture(&manifest, &fitting)
        .expect("fitting succeeds")
        .artifact;
    let mut quiet_builder = SessionFixtureBuilder::new(&manifest, VALIDATION_SESSION_ID);
    quiet_builder.append_held_out_surface_plan(&artifact, 12, false);

    assert!(matches!(
        quiet_builder.finish(
            EvidencePurpose::HeldOutValidation,
            390,
            Some(&fitting.matching_rule),
        ),
        Err(CalibrationCaptureError::Evidence(_))
    ));
}

#[test]
fn held_out_plan_rejects_seed_mutation() {
    let manifest = manifest();
    let fitting = fitting_fixture();
    let artifact = fit_from_fixture(&manifest, &fitting)
        .expect("fitting succeeds")
        .artifact;
    let held_out = validation_fixture(&manifest, &artifact, &fitting.matching_rule);
    let mut changed_seed = held_out.journal.clone();
    let Some(CalibrationSurfacePlanBinding::HeldOutValidation { run_seed, .. }) =
        &mut changed_seed.surface_plan
    else {
        panic!("held-out fixture carries its exact plan binding");
    };
    *run_seed ^= 1;

    assert!(matches!(
        changed_seed.validate_complete_surface_plan(),
        Err(CalibrationCaptureError::SurfacePlanIdentityMismatch)
    ));
}

#[test]
fn held_out_readiness_requires_every_plan_owned_motion_segment() {
    let manifest = manifest();
    let fitting = fitting_fixture();
    let artifact = fit_from_fixture(&manifest, &fitting)
        .expect("fitting succeeds")
        .artifact;
    let held_out = validation_fixture(&manifest, &artifact, &fitting.matching_rule);
    let plan = HeldOutSurfacePlan::v1(
        artifact.fingerprint().expect("artifact hashes"),
        HELDOUT_RUN_SEED,
    );
    let ordinary_prompt_id = plan
        .segments()
        .iter()
        .filter(|segment| segment.kind() == SurfaceSegmentKind::OrdinaryMotion)
        .map(|segment| segment.id().clone())
        .next_back()
        .expect("canonical held-out plan ends with ordinary motion");
    let mut missing_motion = held_out.journal.clone();
    missing_motion.events.retain(|event| {
        !matches!(
            event,
            CapturedCalibrationEvent::Idle { prompt_id, .. }
                if prompt_id == &ordinary_prompt_id
        )
    });

    assert!(matches!(
        missing_motion.validate_complete_surface_plan(),
        Err(CalibrationCaptureError::MissingSurfacePrompt(_))
    ));
}


fn raw_timing_neutral_session(outside_neutral: bool, duplicate_aggregate: bool) -> (FinalizedSession, Vec<NeutralRecordWindow>) {
    let manifest = manifest();
    let mut writer = SessionWriter::new(Vec::new(), SessionHeader {
        nominal_period_us: manifest.nominal_period_us,
        manifest_sha256: manifest.sha256().unwrap(),
        session_id: FIT_SESSION_ID,
    }).unwrap();
    let mut ordinal = 0_u64;
    for hand in [Hand::Left, Hand::Right] {
        let identity = manifest.hands[hand as usize].frame_identity().unwrap();
        let count = if outside_neutral && hand == Hand::Left { 42 } else { 40 };
        for sample in 0..count {
            let mut frame = SensorFrame {
                hand, controller_id_sha256: identity.controller_id_sha256,
                boot_id: identity.boot_id, flags: 0, valid_sensor_mask: ALL_SENSORS_VALID,
                sequence: sample as u32, device_time_us: 100_000 + sample as u64 * 5_000,
                sensors: [SensorReading { acceleration: [0, 0, 16_000], angular_velocity: [0; 3], sample_time_offset_us: 0 }; SENSOR_COUNT],
            };
            if hand == Hand::Left && sample == 1 {
                if duplicate_aggregate {
                    frame.device_time_us = 100_000;
                    for sensor in &mut frame.sensors { sensor.sample_time_offset_us = -999; }
                } else { frame.sensors[0].sample_time_offset_us = -5_000; }
            }
            if duplicate_aggregate && hand == Hand::Left && sample == 0 {
                for sensor in &mut frame.sensors { sensor.sample_time_offset_us = -1_000; }
            }
            writer.append_frame(ControllerStream::from(hand), &frame, 1_000_000_000 + frame.device_time_us * 1_001).unwrap();
            ordinal += 1;
        }
    }
    let session = writer.finish_finalized().unwrap();
    let windows = vec![NeutralRecordWindow {
        block_id: "raw-admission".to_owned(), window_id: "neutral".to_owned(),
        role: NeutralWindowRole::Neutral, start_record: if outside_neutral { 2 } else { 0 },
        end_record_exclusive: ordinal,
    }];
    (session, windows)
}

#[test]
fn raw_sensor_repeat_in_neutral_returns_source_error_without_panicking() {
    let (session, windows) = raw_timing_neutral_session(false, false);
    let clocks = ClockEpochSet::fit_session(&manifest(), &session).expect("canonical 1001ns/us clocks fit");
    clocks.fingerprint().unwrap();
    let error = derive_matching_rule(&manifest(), &session, &windows)
        .expect_err("repeated raw sensor time must return a typed source error before neutral construction");
    let context = format!("{error:?}");
    assert!(context.contains("record_ordinal: 1"), "{context}");
    assert!(context.contains("SensorTimeRegression"), "{context}");
}

#[test]
fn raw_sensor_repeat_outside_neutral_is_rejected_under_1001ns_clocks() {
    let (session, windows) = raw_timing_neutral_session(true, false);
    let clocks = ClockEpochSet::fit_session(&manifest(), &session).expect("canonical 1001ns/us clocks fit");
    clocks.fingerprint().unwrap();
    let error = derive_matching_rule(&manifest(), &session, &windows)
        .expect_err("whole-stream raw admission must include frames outside selected truth");
    let context = format!("{error:?}");
    assert!(context.contains("record_ordinal: 1"), "{context}");
    assert!(context.contains("SensorTimeRegression"), "{context}");
}

#[test]
fn raw_aggregate_repeat_with_advancing_sensor_times_is_rejected() {
    let (session, windows) = raw_timing_neutral_session(true, true);
    let error = derive_matching_rule(&manifest(), &session, &windows)
        .expect_err("aggregate time must strictly advance even when every sensor advances");
    let context = format!("{error:?}");
    assert!(context.contains("record_ordinal: 1"), "{context}");
    assert!(context.contains("TimeRegression"), "{context}");
}


fn append_retimed_raw_boundary(
    manifest: &AcquisitionManifest,
    fixture: &EvidenceFixture,
    slope_ns_per_us: u64,
    repeated_sensor: bool,
) -> (FinalizedSession, ClockEpochSet, CalibrationCaptureJournal, u64) {
    let mut writer = SessionWriter::new(Vec::new(), *fixture.session.header()).unwrap();
    let mut neutral_source_writer = if fixture.journal.purpose == EvidencePurpose::Fitting {
        Some(SessionWriter::new(Vec::new(), *fixture.session.header()).unwrap())
    } else { None };
    let mut reader = fixture.session.reader();
    let mut last = None;
    let mut ordinal = 0_u64;
    while let Some(record) = reader.next_record().unwrap() {
        let mut frame = record.decode().unwrap();
        frame.device_time_us = (record.host_time_ns - 1_000_000_000) / slope_ns_per_us;
        for sensor in &mut frame.sensors { sensor.sample_time_offset_us = 0; }
        writer.append_frame(ControllerStream::from(frame.hand), &frame, record.host_time_ns).unwrap();
        if let Some(neutral_writer) = &mut neutral_source_writer {
            neutral_writer.append_frame(ControllerStream::from(frame.hand), &frame, record.host_time_ns).unwrap();
        }
        last = Some(frame);
        ordinal += 1;
    }
    let mut frame = last.unwrap();
    for sensor in &mut frame.sensors {
        sensor.acceleration = [0, 0, 16_000];
        sensor.angular_velocity = [0; 3];
    }
    let boundary_record = ordinal + 1;
    let mut boundary_start = frame;
    let mut boundary_end = frame;
    for sample in 0..34 {
        frame.sequence = frame.sequence.wrapping_add(1);
        frame.device_time_us += if sample == 1 && !repeated_sensor { 5_001 } else { 5_000 };
        frame.sensors[0].sample_time_offset_us = if sample == 1 { -5_000 } else { 0 };
        if sample == 0 { boundary_start = frame; }
        if sample == 1 { boundary_end = frame; }
        writer.append_frame(ControllerStream::from(frame.hand), &frame,
            1_000_000_000 + frame.device_time_us * slope_ns_per_us).unwrap();
    }
    let session = writer.finish_finalized().unwrap();
    let clocks = ClockEpochSet::fit_session(manifest, &session).expect("recomputed affine raw clocks qualify");
    for hand in [Hand::Left, Hand::Right] {
        assert!((clocks.fit_for(hand).unwrap().report().slope_ns_per_us - slope_ns_per_us as f64).abs() < 0.01);
    }
    assert_eq!(boundary_end.device_time_us - 5_000 - boundary_start.device_time_us,
        u64::from(!repeated_sensor));
    let fit = clocks.fit_for(boundary_start.hand).unwrap();
    let mapped_start = fit.map_device_time_ns(boundary_start.boot_id, boundary_start.device_time_us).unwrap() / 1_000;
    let mapped_end = fit.map_device_time_ns(boundary_end.boot_id, boundary_end.device_time_us).unwrap() / 1_000;
    if repeated_sensor { assert!(mapped_end - 5_000 > mapped_start); }
    else { assert!(mapped_end - 5_000 < mapped_start); }
    let mut journal = fixture.journal.clone();
    journal.session_sha256 = format_sha256(&session.sha256());
    journal.clock_epochs_sha256 = format_sha256(&clocks.fingerprint().unwrap());
    if let Some(neutral_writer) = neutral_source_writer {
        let neutral_source = neutral_writer.finish_finalized().unwrap();
        journal.matching_rule = Some(derive_matching_rule(manifest, &neutral_source,
            &journal.neutral_windows).expect("healthy retimed neutral records bind the fitting rule"));
    }
    (session, clocks, journal, boundary_record)
}

#[test]
fn heldout_999ns_clock_preserves_valid_raw_sensor_advance_at_skew_boundary() {
    let manifest = manifest();
    let fitting = fitting_fixture();
    let artifact = fit_from_fixture(&manifest, &fitting).unwrap().artifact;
    let heldout = validation_fixture(&manifest, &artifact, &fitting.matching_rule);
    let (session, clocks, journal, _) = append_retimed_raw_boundary(&manifest, &heldout, 999, false);
    let validated = validate_calibration(&manifest, &clocks, &artifact, &journal, &session)
        .expect("complete held-out stream keeps raw +1us sample progress at allowed skew under a 999ns/us clock");
    assert!(validated.report.key_state_neutral);
    assert_eq!(validated.report.ordinary_motion_exposure_ms, 900);
}

#[test]
fn heldout_1001ns_clock_rejects_raw_sensor_repeat_outside_truth_windows() {
    let manifest = manifest();
    let fitting = fitting_fixture();
    let artifact = fit_from_fixture(&manifest, &fitting).unwrap().artifact;
    let heldout = validation_fixture(&manifest, &artifact, &fitting.matching_rule);
    let (session, clocks, journal, boundary_record) = append_retimed_raw_boundary(&manifest, &heldout, 1_001, true);
    let error = validate_calibration(&manifest, &clocks, &artifact, &journal, &session)
        .err().expect("unlabeled suffix must not hide repeated raw sensor time under a 1001ns/us clock");
    let context = format!("{error:?}");
    assert!(context.contains(&format!("record_ordinal: {boundary_record}")), "{context}");
    assert!(context.contains("SensorTimeRegression"), "{context}");
}


#[test]
fn fitting_and_precheck_reject_raw_suffix_fault_before_truth_selection() {
    let manifest = manifest();
    let fitting = fitting_fixture();
    let (session, clocks, journal, boundary_record) = append_retimed_raw_boundary(&manifest, &fitting, 1_001, true);
    let precheck = evaluate_keyboard_precheck(&manifest, &clocks, &journal, &session)
        .expect_err("keyboard precheck must admit the whole raw stream before selected windows");
    let fit = fit_calibration(CalibrationFitInput {
        manifest: &manifest, clocks: &clocks, journal: &journal, session: &session,
        neutral_report: fitting.neutral_report.as_ref().unwrap(),
    }).err().expect("fitting must admit the whole raw stream before neutral evidence and selection");
    for context in [format!("{precheck:?}"), format!("{fit:?}")] {
        assert!(context.contains(&format!("record_ordinal: {boundary_record}")), "{context}");
        assert!(context.contains("SensorTimeRegression"), "{context}");
    }
}
