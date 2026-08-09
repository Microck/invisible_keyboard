use std::{cell::RefCell, convert::Infallible, rc::Rc, sync::OnceLock};

use invisible_keyboard::{
    artifact::{
        ArtifactDomain, ArtifactSensorBinding, ArtifactSource, ArtifactSourceRole,
        CalibrationArtifact, CalibrationArtifactInput, HandClockFit, KeyboardIdentity,
        KeyboardLayoutConfirmationReceipt,
    },
    calibration_app::{
        CalibrationApp, EvidenceMode, HardwareCalibrationEvent, KeyboardLayoutDecisionState,
    },
    calibration_capture::{
        CALIBRATION_MATCHING_RULE_SCHEMA, CONTACT_DEBOUNCE_WINDOWS, CONTACT_FEATURE_SCHEMA,
        CONTACT_REARM_WINDOWS, CONTACT_RESPONSE_END_NS, CONTACT_RESPONSE_START_NS,
        CalibrationCaptureInput, CalibrationCaptureJournal, CalibrationMatchingRule,
        CalibrationSurfacePlanBinding, CapturedCalibrationEvent, HandContactThresholds,
        SurfacePresentationReceipt,
    },
    calibration_engine::{EvidencePurpose, validate_calibration},
    clock::{ClockFit, ClockSample, MIN_CLOCK_SPAN_US},
    clock_epoch::ClockEpochSet,
    decoder::{AbstainReason, HandDecoder, MAX_INTER_SENSOR_SKEW_US},
    layout::{Hand, KeyUsage, PHYSICAL_POSITION_COUNT, PhysicalPosition, all_slots},
    live::{
        LIVE_WATCHDOG_NS, LiveCalibratedError, LiveCalibratedRuntime, LiveTerminalReason,
        REVOKED_STATUS, USER_ATTESTED_UNMOVED_STATUS,
    },
    manifest::{
        ACQUISITION_MANIFEST_SCHEMA, AcquisitionManifest, CORE28_LAYOUT_ID, HandManifest,
        ManifestError, ManifestHand, PROTOCOL_VERSION, SensorManifest, SensorRole, format_sha256,
    },
    output::{EventState, KeyEvent, OutputAdapter, OutputReceipt, RecordingOutput, ShutdownSignal},
    prompts::{HeldOutSurfacePlan, PromptCorpus, SurfaceSegmentKind},
    protocol::{
        ALL_SENSORS_VALID, FLAG_ACQUISITION_OVERRUN, FLAG_CLOCK_UNSYNCED, QuiesceRequest,
        SensorFrame, decode_frame, encode_frame,
    },
    replay::{
        CalibratedReplayError, CalibratedRuntimeFault, MAX_MERGE_PENDING_RECORDS,
        RuntimeBindingError, run_calibrated_replay, verify_runtime_binding,
    },
    runtime::{RuntimeOutputError, TransportFault},
    session::{
        ControllerStream, FinalizedSession, SessionHeader, SessionReader, SessionRecordKind,
        SessionWriter,
    },
    simulation::SyntheticConfig,
    transport::TransportCandidate,
    validation::{HeldOutValidation, WearSessionConfirmation},
};

const SESSION_ID: [u8; 16] = [0x77; 16];
const VALIDATION_SESSION_ID: [u8; 16] = [0x78; 16];
const VALIDATION_RUN_SEED: u64 = 73;
const VALIDATION_PLAN_START_NS: u64 = 10_000_000_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LiveOutputError {
    Startup,
    Transition,
    Neutralization,
}

struct FaultInjectingLiveOutput {
    fail_startup: bool,
    fail_neutralization: bool,
    pressed: bool,
    release_all_calls: u64,
}

/// A real recording adapter with a shared observation handle for error paths
/// that consume the runtime and its output.
#[derive(Clone, Default)]
struct SharedRecordingOutput {
    inner: Rc<RefCell<RecordingOutput>>,
}

impl SharedRecordingOutput {
    fn event_count(&self) -> usize {
        self.inner.borrow().events().len()
    }

    fn release_all_calls(&self) -> u64 {
        self.inner.borrow().release_all_calls()
    }
}

impl OutputAdapter for SharedRecordingOutput {
    type Error = Infallible;

    fn emit(&mut self, event: KeyEvent) -> Result<OutputReceipt, Self::Error> {
        self.inner.borrow_mut().emit(event)
    }

    fn release_all(&mut self) -> Result<(), Self::Error> {
        self.inner.borrow_mut().release_all()
    }
}

impl OutputAdapter for FaultInjectingLiveOutput {
    type Error = LiveOutputError;

    fn emit(&mut self, event: KeyEvent) -> Result<OutputReceipt, Self::Error> {
        match event.state {
            EventState::Down => self.pressed = true,
            EventState::Up => return Err(LiveOutputError::Transition),
        }
        Ok(OutputReceipt { event, ordinal: 0 })
    }

    fn release_all(&mut self) -> Result<(), Self::Error> {
        if self.fail_startup && self.release_all_calls == 0 {
            return Err(LiveOutputError::Startup);
        }
        self.release_all_calls += 1;
        if self.fail_neutralization && self.release_all_calls > 1 {
            return Err(LiveOutputError::Neutralization);
        }
        self.pressed = false;
        Ok(())
    }
}

#[derive(Clone)]
struct ReplayFixture {
    calibration_manifest: AcquisitionManifest,
    runtime_manifest: AcquisitionManifest,
    clocks: ClockEpochSet,
    artifact: CalibrationArtifact,
    held_out_validation: HeldOutValidation,
    session: FinalizedSession,
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
        sensors: std::array::from_fn(|sensor_index| {
            let role = [
                SensorRole::Thumb,
                SensorRole::Index,
                SensorRole::Middle,
                SensorRole::Ring,
                SensorRole::Pinky,
                SensorRole::Dorsal,
            ][sensor_index];
            let sensor_index = u8::try_from(sensor_index).expect("fixture sensor index fits u8");
            SensorManifest {
                sensor_index,
                mux_channel: sensor_index,
                role,
                mount_id: format!("{side}-{role:?}-mount-v1").to_lowercase(),
                wiring_id: format!("{side}-{role:?}-wiring-v1").to_lowercase(),
                interrupt_pin: format!("{side}-int{sensor_index}"),
                axis_map: [1, 0, 0, 0, 1, 0, 0, 0, 1],
            }
        }),
    }
}

fn artifact_sensor_bindings(manifest: &AcquisitionManifest) -> Vec<ArtifactSensorBinding> {
    manifest
        .hands
        .iter()
        .flat_map(|hand| {
            let artifact_hand = match hand.hand {
                ManifestHand::Left => Hand::Left,
                ManifestHand::Right => Hand::Right,
            };
            hand.sensors
                .iter()
                .cloned()
                .map(move |sensor| ArtifactSensorBinding {
                    hand: artifact_hand,
                    sensor,
                })
        })
        .collect()
}

fn base_manifest(left_boot: u8, right_boot: u8) -> AcquisitionManifest {
    AcquisitionManifest {
        schema: ACQUISITION_MANIFEST_SCHEMA.to_owned(),
        layout_id: CORE28_LAYOUT_ID.to_owned(),
        protocol_version: PROTOCOL_VERSION,
        hands: vec![
            hand_manifest(ManifestHand::Left, left_boot),
            hand_manifest(ManifestHand::Right, right_boot),
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
        study_id: "calibration-training".to_owned(),
        split_id: "training".to_owned(),
        participant_alias: "fixture-operator".to_owned(),
        consent_record: "fixture-consent".to_owned(),
        retention_policy: "local-delete-after-analysis-v1".to_owned(),
        rig_evidence_sha256: format_sha256(&[0xbb; 32]),
        axis_characterization_sha256: format_sha256(&[0xcc; 32]),
        calibration_artifact_sha256: None,
        calibration_validation_sha256: None,
    }
}

fn fit_for_times(boot: u8, times: &[u64], offset_ns: u64) -> ClockFit {
    let samples = times
        .iter()
        .copied()
        .map(|device_time_us| ClockSample {
            device_time_us,
            host_time_ns: offset_ns + device_time_us * 1_000,
        })
        .collect::<Vec<_>>();
    ClockFit::fit([boot; 16], &samples).expect("fixture clock fits")
}

fn calibration_matching_rule(manifest: &AcquisitionManifest) -> CalibrationMatchingRule {
    let rule = CalibrationMatchingRule {
        schema: CALIBRATION_MATCHING_RULE_SCHEMA.to_owned(),
        acquisition_manifest_sha256: format_sha256(
            &manifest.sha256().expect("matching-rule manifest hashes"),
        ),
        neutral_windows_sha256: format_sha256(&[0x41; 32]),
        neutral_source_sha256: format_sha256(&[0x42; 32]),
        feature_schema: CONTACT_FEATURE_SCHEMA.to_owned(),
        thresholds: [
            HandContactThresholds {
                hand: Hand::Left,
                low_energy: 12_500_000,
                high_energy: 50_000_000,
                motion_energy: 4_096,
            },
            HandContactThresholds {
                hand: Hand::Right,
                low_energy: 12_500_000,
                high_energy: 50_000_000,
                motion_energy: 4_096,
            },
        ],
        debounce_windows: CONTACT_DEBOUNCE_WINDOWS,
        rearm_windows: CONTACT_REARM_WINDOWS,
        response_start_ns: CONTACT_RESPONSE_START_NS,
        response_end_ns: CONTACT_RESPONSE_END_NS,
    };
    rule.validate().expect("matching-rule fixture is valid");
    rule
}

fn build_artifact(
    manifest: &AcquisitionManifest,
    left: HandDecoder,
    right: HandDecoder,
) -> CalibrationArtifact {
    let calibration_times = (0..24_u64).map(|index| index * 5_000).collect::<Vec<_>>();
    CalibrationArtifact::new(CalibrationArtifactInput {
        acquisition_manifest_sha256: manifest.sha256().expect("manifest hashes"),
        fitting_capture_journal_sha256: [31; 32],
        fitting_evidence_sha256: [32; 32],
        prompt_corpus_sha256: PromptCorpus::v1().sha256(),
        neutral_evidence_sha256: [3; 32],
        calibration_matching_rule_sha256: calibration_matching_rule(manifest)
            .fingerprint()
            .expect("matching-rule fixture hashes"),
        sensor_bindings: artifact_sensor_bindings(manifest),
        keyboard: KeyboardIdentity {
            name: "Fixture USB Keyboard".to_owned(),
            physical_path: "fixture/input0".to_owned(),
            vendor_id: 0x1234,
            product_id: 0x5678,
            logical_layout: "us-qwerty-v1".to_owned(),
            layout_confirmation: KeyboardLayoutConfirmationReceipt::us_qwerty(1, 1),
        },
        sources: vec![
            ArtifactSource::new(
                [10; 32],
                ArtifactSourceRole::Training,
                ArtifactDomain::Keyboard,
                vec![10],
            ),
            ArtifactSource::new(
                [11; 32],
                ArtifactSourceRole::Training,
                ArtifactDomain::PassiveSurface,
                vec![20],
            ),
            ArtifactSource::new(
                [12; 32],
                ArtifactSourceRole::ThresholdSelection,
                ArtifactDomain::PassiveSurface,
                vec![30],
            ),
        ],
        clock_fits: vec![
            HandClockFit {
                hand: Hand::Left,
                fit: fit_for_times(0x11, &calibration_times, 1_000_000_000),
            },
            HandClockFit {
                hand: Hand::Right,
                fit: fit_for_times(0x22, &calibration_times, 1_001_000_000),
            },
        ],
        left_decoder: Some(left),
        right_decoder: Some(right),
        accepted_position_counts: vec![4; PHYSICAL_POSITION_COUNT],
        rejected_examples: 0,
        setup_seconds: 90,
        created_unix_ns: 1_786_233_600_000_000_000,
        requires_wear_session_confirmation: true,
    })
    .expect("artifact is complete")
}

fn build_held_out_validation(
    manifest: &AcquisitionManifest,
    artifact: &CalibrationArtifact,
) -> HeldOutValidation {
    build_held_out_validation_with_rule(manifest, artifact, calibration_matching_rule(manifest))
}

fn build_held_out_validation_with_rule(
    manifest: &AcquisitionManifest,
    artifact: &CalibrationArtifact,
    matching_rule: CalibrationMatchingRule,
) -> HeldOutValidation {
    let manifest_sha256 = manifest.sha256().expect("validation manifest hashes");
    let artifact_sha256 = artifact.fingerprint().expect("validation artifact hashes");
    let plan = HeldOutSurfacePlan::v1(artifact_sha256, VALIDATION_RUN_SEED);
    let mut capture = HeldoutCaptureBuilder::new(manifest);
    let events = capture.capture_plan(&plan);
    let session = capture.finish();
    let clocks = ClockEpochSet::fit_session(manifest, &session)
        .expect("held-out clocks fit from the finalized session");
    let journal = CalibrationCaptureJournal::new(CalibrationCaptureInput {
        purpose: EvidencePurpose::HeldOutValidation,
        acquisition_manifest_sha256: manifest_sha256,
        clock_epochs_sha256: clocks.fingerprint().expect("held-out clocks hash"),
        session_sha256: session.sha256(),
        session_id: VALIDATION_SESSION_ID,
        keyboard_device_id: artifact.keyboard.physical_path.clone(),
        keyboard: artifact.keyboard.clone(),
        setup_seconds: 390,
        created_unix_ns: 1_786_233_600_000_000_000,
        pre_rejected_examples: 0,
        neutral_windows: Vec::new(),
        matching_rule: Some(matching_rule),
        surface_plan: Some(
            CalibrationSurfacePlanBinding::held_out(&plan, VALIDATION_PLAN_START_NS)
                .expect("held-out plan binding is valid"),
        ),
        events,
    })
    .expect("held-out capture journal is complete");
    journal
        .validate_complete_surface_plan()
        .expect("held-out capture covers the immutable plan");
    let evaluated = validate_calibration(manifest, &clocks, artifact, &journal, &session)
        .expect("raw held-out capture evaluates");
    assert_eq!(evaluated.report.ordinary_motion_exposure_ms, 60_000);
    evaluated
        .held_out_validation()
        .expect("raw held-out capture passes every admission threshold")
}

struct HeldoutCaptureBuilder<'a> {
    manifest: &'a AcquisitionManifest,
    writer: SessionWriter<Vec<u8>>,
    trials: [u64; 2],
    sequences: [u32; 2],
    next_host_time_ns: u64,
}

impl<'a> HeldoutCaptureBuilder<'a> {
    fn new(manifest: &'a AcquisitionManifest) -> Self {
        let writer = SessionWriter::new(
            Vec::new(),
            SessionHeader {
                nominal_period_us: manifest.nominal_period_us,
                manifest_sha256: manifest.sha256().expect("held-out manifest hashes"),
                session_id: VALIDATION_SESSION_ID,
            },
        )
        .expect("held-out session starts");
        Self {
            manifest,
            writer,
            trials: [0, 0],
            sequences: [0, 0],
            next_host_time_ns: VALIDATION_PLAN_START_NS,
        }
    }

    fn append_window(
        &mut self,
        hand: Hand,
        position: Option<PhysicalPosition>,
        not_before_host_time_ns: u64,
    ) -> u64 {
        let hand_index = hand as usize;
        let identity = self.manifest.hands[hand_index]
            .frame_identity()
            .expect("held-out manifest has wire identity");
        let seed = match hand {
            Hand::Left => 0x0052_504c_594c,
            Hand::Right => 0x0052_504c_5952,
        };
        let config = SyntheticConfig::new(hand, seed)
            .with_noise(0)
            .with_wire_identity(identity.controller_id_sha256, identity.boot_id);
        let trial = self.trials[hand_index];
        self.trials[hand_index] = self.trials[hand_index].saturating_add(1);
        let mut frames = position.map_or_else(
            || config.idle_window(trial).frames().to_vec(),
            |position| config.tap_window(position, trial).frames().to_vec(),
        );
        canonical_vectors_to_wire(self.manifest, &mut frames);
        self.next_host_time_ns = self.next_host_time_ns.max(not_before_host_time_ns);
        let mut trigger_host_time_ns = 0;
        for mut frame in frames {
            frame.sequence = self.sequences[hand_index];
            self.sequences[hand_index] = self.sequences[hand_index].wrapping_add(1);
            frame.device_time_us = self.next_host_time_ns / 1_000;
            trigger_host_time_ns = self.next_host_time_ns;
            self.writer
                .append_frame(ControllerStream::from(hand), &frame, trigger_host_time_ns)
                .expect("held-out frame writes");
            self.next_host_time_ns = self
                .next_host_time_ns
                .saturating_add(u64::from(self.manifest.nominal_period_us) * 1_000);
        }
        trigger_host_time_ns
    }

    fn capture_plan(&mut self, plan: &HeldOutSurfacePlan) -> Vec<CapturedCalibrationEvent> {
        let mut events = Vec::new();
        let mut instruction_revision = 0_u64;
        let mut ordinary_segments_seen = [0_u32; 2];
        for segment in plan.segments() {
            let segment_start_ns = VALIDATION_PLAN_START_NS.saturating_add(segment.start_ns());
            for scheduled_presentation in segment.scheduled_presentations() {
                let presentation_host_time_ns =
                    VALIDATION_PLAN_START_NS.saturating_add(scheduled_presentation.at_ns());
                instruction_revision = instruction_revision.saturating_add(1);
                events.push(CapturedCalibrationEvent::DisplayAcknowledged {
                    prompt_id: segment.id().clone(),
                    presentation: SurfacePresentationReceipt::new(
                        presentation_host_time_ns,
                        presentation_host_time_ns,
                        presentation_host_time_ns,
                        instruction_revision,
                    )
                    .expect("surface presentation receipt is ordered"),
                });
            }
            match segment.kind() {
                SurfaceSegmentKind::PromptedTap => {
                    let position = plan
                        .scoring_targets()
                        .iter()
                        .find(|target| target.prompt_id() == segment.id())
                        .expect("every prompted segment has scoring metadata")
                        .position();
                    let cue_host_time_ns = VALIDATION_PLAN_START_NS.saturating_add(
                        segment
                            .cue_at_ns()
                            .expect("prompted surface segment has a cue"),
                    );
                    self.append_window(position.hand, None, cue_host_time_ns);
                    self.append_window(position.hand, None, 0);
                    self.append_window(position.hand, Some(position), 0);
                    // The impulse lands at the tail of the synthetic tap.
                    // Keep the physical stream continuous so the frozen
                    // two-window debounce completes inside the response gate.
                    self.append_window(position.hand, None, 0);
                }
                SurfaceSegmentKind::QuietNull => {
                    let left_trigger = self.append_window(Hand::Left, None, segment_start_ns);
                    events.push(CapturedCalibrationEvent::Idle {
                        prompt_id: segment.id().clone(),
                        sample_ordinal: 0,
                        trigger_host_time_ns: left_trigger,
                    });
                    let right_trigger = self.append_window(Hand::Right, None, 0);
                    events.push(CapturedCalibrationEvent::Idle {
                        prompt_id: segment.id().clone(),
                        sample_ordinal: 1,
                        trigger_host_time_ns: right_trigger,
                    });
                }
                SurfaceSegmentKind::OrdinaryMotion => {
                    let hand = segment
                        .motion_hand()
                        .expect("ordinary-motion segment names one hand");
                    let hand_index = hand as usize;
                    let sample_count = if ordinary_segments_seen[hand_index] < 2 {
                        267
                    } else {
                        266
                    };
                    ordinary_segments_seen[hand_index] += 1;
                    for sample_ordinal in 0..sample_count {
                        let trigger_host_time_ns = self.append_window(hand, None, segment_start_ns);
                        events.push(CapturedCalibrationEvent::Idle {
                            prompt_id: segment.id().clone(),
                            sample_ordinal,
                            trigger_host_time_ns,
                        });
                    }
                }
                SurfaceSegmentKind::ThresholdTap | SurfaceSegmentKind::ThresholdQuiet => {
                    panic!("held-out plan cannot contain fitting-only segments")
                }
            }
        }
        events
    }

    fn finish(self) -> FinalizedSession {
        self.writer
            .finish_finalized()
            .expect("held-out session finalizes and audits")
    }
}

fn collect_windows(
    config: &SyntheticConfig,
    position: invisible_keyboard::layout::PhysicalPosition,
) -> Vec<SensorFrame> {
    let mut frames = [
        config.idle_window(100),
        config.idle_window(101),
        config.idle_window(102),
        config.tap_window(position, 103),
        config.idle_window(104),
    ]
    .into_iter()
    .flat_map(|window| window.frames().to_vec())
    .collect::<Vec<_>>();

    // Synthetic trial IDs normally place windows one second apart. A live
    // transport fixture must instead model the manifest's continuous 200 Hz
    // stream so watchdog behavior reflects a connected controller.
    let first_device_time_us = frames
        .first()
        .expect("synthetic live stream contains frames")
        .device_time_us;
    for (index, frame) in frames.iter_mut().enumerate() {
        frame.device_time_us =
            first_device_time_us + u64::try_from(index).expect("fixture index fits u64") * 5_000;
    }
    frames
}

fn fixture() -> ReplayFixture {
    static FIXTURE: OnceLock<ReplayFixture> = OnceLock::new();
    FIXTURE.get_or_init(|| build_fixture(false)).clone()
}

fn fixture_with_axis_rotation(rotated_axes: bool) -> ReplayFixture {
    if !rotated_axes {
        return fixture();
    }
    static ROTATED_FIXTURE: OnceLock<ReplayFixture> = OnceLock::new();
    ROTATED_FIXTURE.get_or_init(|| build_fixture(true)).clone()
}

fn build_fixture(rotated_axes: bool) -> ReplayFixture {
    let left_config = SyntheticConfig::new(Hand::Left, 0x0052_504c_594c);
    let right_config = SyntheticConfig::new(Hand::Right, 0x0052_504c_5952);
    let left_decoder = HandDecoder::calibrate(Hand::Left, &left_config.calibration_examples(5, 20))
        .expect("left calibration fits");
    let right_decoder =
        HandDecoder::calibrate(Hand::Right, &right_config.calibration_examples(5, 20))
            .expect("right calibration fits");
    let mut calibration_manifest = base_manifest(0x11, 0x22);
    if rotated_axes {
        install_nonidentity_axis_maps(&mut calibration_manifest);
    }
    let artifact = build_artifact(
        &calibration_manifest,
        left_decoder.clone(),
        right_decoder.clone(),
    );
    let held_out_validation = build_held_out_validation(&calibration_manifest, &artifact);
    let mut runtime_manifest = base_manifest(0x31, 0x32);
    if rotated_axes {
        install_nonidentity_axis_maps(&mut runtime_manifest);
    }
    runtime_manifest.study_id = "calibrated-runtime-replay".to_owned();
    runtime_manifest.split_id = "held-out-runtime".to_owned();
    runtime_manifest.calibration_artifact_sha256 = Some(format_sha256(
        &artifact.fingerprint().expect("artifact hashes"),
    ));
    runtime_manifest.calibration_validation_sha256 = Some(format_sha256(
        &held_out_validation.validation_report_sha256(),
    ));
    let left_runtime_identity = runtime_manifest.hands[0]
        .frame_identity()
        .expect("left runtime identity exists");
    let right_runtime_identity = runtime_manifest.hands[1]
        .frame_identity()
        .expect("right runtime identity exists");
    let left_runtime_config = left_config.with_wire_identity(
        left_runtime_identity.controller_id_sha256,
        left_runtime_identity.boot_id,
    );
    let right_runtime_config = right_config.with_wire_identity(
        right_runtime_identity.controller_id_sha256,
        right_runtime_identity.boot_id,
    );

    let left_position = all_slots()
        .iter()
        .find(|slot| slot.label == "A")
        .expect("A exists")
        .position;
    let right_position = all_slots()
        .iter()
        .find(|slot| slot.label == "H")
        .expect("H exists")
        .position;
    let mut left_frames = collect_windows(&left_runtime_config, left_position);
    let mut right_frames = collect_windows(&right_runtime_config, right_position);
    if rotated_axes {
        canonical_vectors_to_wire(&runtime_manifest, &mut left_frames);
        canonical_vectors_to_wire(&runtime_manifest, &mut right_frames);
    }
    let left_times = left_frames
        .iter()
        .map(|frame| frame.device_time_us)
        .collect::<Vec<_>>();
    let right_times = right_frames
        .iter()
        .map(|frame| frame.device_time_us)
        .collect::<Vec<_>>();
    let left_fit = fit_for_times(0x31, &left_times, 5_000_000_000);
    let right_fit = fit_for_times(0x32, &right_times, 5_001_000_000);
    let runtime_hash = runtime_manifest.sha256().expect("runtime manifest hashes");
    let header = SessionHeader {
        nominal_period_us: runtime_manifest.nominal_period_us,
        manifest_sha256: runtime_hash,
        session_id: SESSION_ID,
    };
    let mut timed_frames = left_frames
        .into_iter()
        .map(|frame| {
            let host = left_fit
                .map_device_time_ns([0x31; 16], frame.device_time_us)
                .expect("left time maps");
            (host, frame)
        })
        .chain(right_frames.into_iter().map(|frame| {
            let host = right_fit
                .map_device_time_ns([0x32; 16], frame.device_time_us)
                .expect("right time maps");
            (host, frame)
        }))
        .collect::<Vec<_>>();
    timed_frames.sort_by_key(|(host, frame)| (*host, frame.hand as u8, frame.sequence));
    let mut writer = SessionWriter::new(Vec::new(), header).expect("session starts");
    for (host, frame) in timed_frames {
        writer
            .append_frame(ControllerStream::from(frame.hand), &frame, host)
            .expect("runtime frame writes");
    }
    let session = writer
        .finish_finalized()
        .expect("session finishes and audits");
    let clocks = ClockEpochSet::fit_session(&runtime_manifest, &session)
        .expect("runtime clocks derive exactly from preserved receipts");

    ReplayFixture {
        calibration_manifest,
        runtime_manifest,
        clocks,
        artifact,
        held_out_validation,
        session,
    }
}

fn confirm_wearables_unmoved(fixture: &ReplayFixture) -> WearSessionConfirmation {
    fixture
        .held_out_validation
        .user_attests_both_wearables_have_not_moved()
}

fn install_nonidentity_axis_maps(manifest: &mut AcquisitionManifest) {
    const FINGER_TO_CANONICAL: [i8; 9] = [0, -1, 0, 1, 0, 0, 0, 0, 1];
    const DORSAL_TO_CANONICAL: [i8; 9] = [1, 0, 0, 0, 0, -1, 0, 1, 0];
    for hand in &mut manifest.hands {
        for sensor in &mut hand.sensors {
            sensor.axis_map = if sensor.role == SensorRole::Dorsal {
                DORSAL_TO_CANONICAL
            } else {
                FINGER_TO_CANONICAL
            };
        }
    }
}

fn canonical_vectors_to_wire(manifest: &AcquisitionManifest, frames: &mut [SensorFrame]) {
    for frame in frames {
        let hand = manifest
            .hands
            .iter()
            .find(|candidate| {
                matches!(
                    (candidate.hand, frame.hand),
                    (ManifestHand::Left, Hand::Left) | (ManifestHand::Right, Hand::Right)
                )
            })
            .expect("fixture frame hand is declared");
        for (reading, sensor) in frame.sensors.iter_mut().zip(&hand.sensors) {
            let canonical_to_wire = transpose_axis_map(sensor.axis_map);
            reading.acceleration = rotate_fixture_vector(canonical_to_wire, reading.acceleration);
            reading.angular_velocity =
                rotate_fixture_vector(canonical_to_wire, reading.angular_velocity);
        }
    }
}

fn transpose_axis_map(map: [i8; 9]) -> [i8; 9] {
    std::array::from_fn(|index| {
        let row = index / 3;
        let column = index % 3;
        map[column * 3 + row]
    })
}

fn rotate_fixture_vector(map: [i8; 9], vector: [i16; 3]) -> [i16; 3] {
    std::array::from_fn(|row| {
        let value = (0..3)
            .map(|column| i32::from(map[row * 3 + column]) * i32::from(vector[column]))
            .sum::<i32>();
        i16::try_from(value).expect("synthetic fixture rotation stays in i16")
    })
}

fn push_live_frame(
    live: &mut LiveCalibratedRuntime<RecordingOutput>,
    frame: &SensorFrame,
    receipt_time_ns: u64,
) {
    push_live_candidate(
        &mut *live,
        TransportCandidate::Decoded(
            encode_frame(frame)
                .expect("valid live frame encodes")
                .to_vec(),
        ),
        receipt_time_ns,
    )
    .expect("live frame succeeds");
}

struct ArmedLiveState {
    last_left_frame: SensorFrame,
    last_right_frame: SensorFrame,
    last_receipt_time_ns: u64,
}

fn warm_live_until_armed(
    live: &mut LiveCalibratedRuntime<RecordingOutput>,
    reader: &mut SessionReader<'_>,
) -> ArmedLiveState {
    let mut last_left = None;
    let mut last_right = None;
    let mut last_receipt_time_ns = 0;

    while live.report().armed_transitions == 0 {
        let record = reader
            .next_record()
            .expect("record reads")
            .expect("warmup record exists");
        let frame = decode_frame(&record.payload).expect("session frame decodes");
        match frame.hand {
            Hand::Left => last_left = Some(frame),
            Hand::Right => last_right = Some(frame),
        }
        last_receipt_time_ns = record.host_time_ns;
        push_live_candidate(
            &mut *live,
            TransportCandidate::Decoded(record.payload),
            record.host_time_ns,
        )
        .expect("warmup candidate succeeds");
    }

    ArmedLiveState {
        last_left_frame: last_left.expect("left hand warmed before arming"),
        last_right_frame: last_right.expect("right hand warmed before arming"),
        last_receipt_time_ns,
    }
}

#[test]
fn live_interleaved_retained_impulse_commits_preserve_healthy_output() {
    let fixture = fixture();
    let mut live = LiveCalibratedRuntime::new(
        &fixture.calibration_manifest,
        &fixture.runtime_manifest,
        &fixture.artifact,
        confirm_wearables_unmoved(&fixture),
        RecordingOutput::new(),
    )
    .expect("typed calibrated live admission succeeds");
    let armed = warm_live_until_armed(&mut live, &mut fixture.session.reader());
    let mut left = armed.last_left_frame;
    let mut right = armed.last_right_frame;
    let configs = [
        SyntheticConfig::new(Hand::Left, 0x0052_504c_594c).with_noise(0),
        SyntheticConfig::new(Hand::Right, 0x0052_504c_5952).with_noise(0),
    ];
    let positions = ["A", "H"].map(|label| {
        all_slots()
            .iter()
            .find(|slot| slot.label == label)
            .unwrap()
            .position
    });
    let impulses = std::array::from_fn::<_, 2, _>(|index| {
        configs[index].tap_window(positions[index], 100).frames()[15].sensors
    });
    let idle =
        std::array::from_fn::<_, 2, _>(|index| configs[index].idle_window(101).frames()[0].sensors);
    let mut receipt_time_ns = armed.last_receipt_time_ns - 1_000_000;
    // Arming admits the last right warmup frame into its empty pipeline. Thus
    // its first full window and commit precede the left ones by one sample.
    // Left's only impulse is the oldest sample of its first full window and
    // leaves that window during the delay; right stays active at its commit.
    for index in 0..24 {
        advance_test_frame(&mut left);
        advance_test_frame(&mut right);
        left.sensors = if index == 0 { impulses[0] } else { idle[0] };
        right.sensors = if index == 14 { impulses[1] } else { idle[1] };
        receipt_time_ns += 5_000_000;
        push_live_frame(&mut live, &left, receipt_time_ns);
        push_live_frame(&mut live, &right, receipt_time_ns + 1_000_000);
    }
    assert_eq!(live.report().terminal_reason, None);
    assert!(live.report().runtime_faults.is_empty());
    let execution = live.finish().expect("healthy interleaved stream finishes");
    assert_eq!(execution.report.key_decisions, 2);
    assert_eq!(
        execution
            .output
            .events()
            .iter()
            .filter(|event| event.state == EventState::Down)
            .map(|event| event.usage)
            .collect::<Vec<_>>(),
        [KeyUsage::H, KeyUsage::A]
    );
    assert!(execution.report.finished_cleanly);
    assert!(execution.report.final_key_state_neutral);
    assert!(execution.report.output_neutrality_confirmed);
}

fn prepare_live_clock_warmup<A: OutputAdapter>(
    live: &mut LiveCalibratedRuntime<A>,
    reader: &mut SessionReader<'_>,
    one_hand_ready: bool,
) {
    let mut first_left_time_us = None;
    let mut accepted = 0;
    loop {
        let record = reader
            .next_record()
            .expect("warmup record reads")
            .expect("fixture contains clean warmup");
        let frame = decode_frame(&record.payload).expect("warmup frame decodes");
        if one_hand_ready && frame.hand != Hand::Left {
            continue;
        }
        push_live_candidate(
            &mut *live,
            TransportCandidate::Decoded(record.payload),
            record.host_time_ns,
        )
        .unwrap_or_else(|_| panic!("clean warmup must succeed"));
        accepted += 1;
        if one_hand_ready {
            let first = *first_left_time_us.get_or_insert(frame.device_time_us);
            if frame.device_time_us - first >= MIN_CLOCK_SPAN_US {
                break;
            }
        } else if accepted == 2 {
            break;
        }
    }
    assert_eq!(live.report().armed_transitions, 0);
    assert_eq!(live.report().output_events, 0);
    assert_eq!(live.report().terminal_reason, None);
}

fn assert_live_warmup_quality_faults_revoke(one_hand_ready: bool) {
    let fixture = fixture();
    let mut control = LiveCalibratedRuntime::new(
        &fixture.calibration_manifest,
        &fixture.runtime_manifest,
        &fixture.artifact,
        confirm_wearables_unmoved(&fixture),
        RecordingOutput::new(),
    )
    .expect("control live binding succeeds");
    prepare_live_clock_warmup(&mut control, &mut fixture.session.reader(), one_hand_ready);
    let control = control.finish().expect("clean warmup finishes neutral");
    assert_eq!(control.left_clock_fit.is_some(), one_hand_ready);
    assert!(control.right_clock_fit.is_none());
    assert!(control.output.events().is_empty());

    let cases = [
        (ALL_SENSORS_VALID & !1, 0, 0, AbstainReason::MissingSensor),
        (
            ALL_SENSORS_VALID,
            FLAG_CLOCK_UNSYNCED,
            0,
            AbstainReason::AcquisitionFault,
        ),
        (
            ALL_SENSORS_VALID,
            FLAG_ACQUISITION_OVERRUN,
            0,
            AbstainReason::AcquisitionFault,
        ),
        (
            ALL_SENSORS_VALID,
            0,
            MAX_INTER_SENSOR_SKEW_US + 1,
            AbstainReason::TimingSkew,
        ),
    ];
    for (mask, flags, skew_us, reason) in cases {
        for hand in [Hand::Left, Hand::Right] {
            let mut live = LiveCalibratedRuntime::new(
                &fixture.calibration_manifest,
                &fixture.runtime_manifest,
                &fixture.artifact,
                confirm_wearables_unmoved(&fixture),
                RecordingOutput::new(),
            )
            .expect("live binding succeeds");
            let mut reader = fixture.session.reader();
            prepare_live_clock_warmup(&mut live, &mut reader, one_hand_ready);
            let record = loop {
                let record = reader
                    .next_record()
                    .expect("fault record reads")
                    .expect("fixture contains the fault hand");
                if decode_frame(&record.payload)
                    .expect("fault frame decodes")
                    .hand
                    == hand
                {
                    break record;
                }
            };
            let mut frame = decode_frame(&record.payload).expect("fault frame decodes");
            assert!(
                fixture
                    .runtime_manifest
                    .hands
                    .iter()
                    .any(|binding| binding.frame_identity() == Some(frame.identity())),
                "fault frame retains the runtime wire identity"
            );
            frame.valid_sensor_mask = mask;
            frame.flags = flags;
            frame.sensors[0].sample_time_offset_us =
                i16::try_from(skew_us).expect("test skew fits the wire field");
            let terminal = LiveTerminalReason::EvidenceInvalidatingAbstention(reason);
            assert!(
                matches!(
                    push_live_candidate(&mut live,
                        TransportCandidate::Decoded(
                            encode_frame(&frame).expect("integrity fault encodes").to_vec()
                        ),
                        record.host_time_ns,
                    ),
                    Err(LiveCalibratedError::TerminalFault(actual)) if actual == terminal
                ),
                "hand={hand:?}, flags={flags}, skew={skew_us}, one_hand_ready={one_hand_ready}"
            );
            assert_eq!(live.report().wear_session_status(), REVOKED_STATUS);
            assert_eq!(live.report().terminal_reason, Some(terminal.clone()));
            assert_eq!(live.report().armed_transitions, 0);
            assert!(live.report().output_neutrality_confirmed);
            assert!(matches!(
                push_live_candidate(&mut live,
                    TransportCandidate::Decoded(record.payload),
                    record.host_time_ns,
                ),
                Err(LiveCalibratedError::WearSessionRevoked(actual)) if actual == terminal
            ));
            while let Some(record) = reader.next_record().expect("later clean record reads") {
                assert!(matches!(
                    push_live_candidate(&mut live,
                        TransportCandidate::Decoded(record.payload),
                        record.host_time_ns,
                    ),
                    Err(LiveCalibratedError::WearSessionRevoked(actual)) if actual == terminal
                ));
            }
            assert!(matches!(
                live.disconnect(),
                Err(LiveCalibratedError::WearSessionRevoked(actual)) if actual == terminal
            ));
            let execution = live.finish().expect("revoked warmup remains inspectable");
            assert_eq!(execution.report.terminal_reason, Some(terminal));
            assert_eq!(execution.report.armed_transitions, 0);
            assert_eq!(execution.report.key_decisions, 0);
            assert_eq!(execution.report.output_events, 0);
            assert_eq!(execution.report.invalid_candidates, 0);
            assert!(execution.report.runtime_faults.is_empty());
            assert!(!execution.report.finished_cleanly);
            assert!(execution.report.final_key_state_neutral);
            assert!(execution.report.output_neutrality_confirmed);
            assert_eq!(execution.neutralization_failure, None);
            assert!(execution.left_clock_fit.is_none());
            assert!(execution.right_clock_fit.is_none());
            assert!(execution.output.events().is_empty());
            assert!(execution.output.release_all_calls() >= 2);
        }
    }
}

#[test]
fn live_acquisition_integrity_faults_during_initial_clock_warmup_terminally_revoke() {
    assert_live_warmup_quality_faults_revoke(false);
}

#[test]
fn live_acquisition_integrity_faults_with_only_one_clock_ready_terminally_revoke() {
    assert_live_warmup_quality_faults_revoke(true);
}

#[test]
fn live_warmup_integrity_preserves_primary_fault_when_neutralization_fails() {
    let fixture = fixture();
    for one_hand_ready in [false, true] {
        let mut live = LiveCalibratedRuntime::new(
            &fixture.calibration_manifest,
            &fixture.runtime_manifest,
            &fixture.artifact,
            confirm_wearables_unmoved(&fixture),
            FaultInjectingLiveOutput {
                fail_startup: false,
                fail_neutralization: true,
                pressed: false,
                release_all_calls: 0,
            },
        )
        .expect("live startup reaches neutral");
        let mut reader = fixture.session.reader();
        prepare_live_clock_warmup(&mut live, &mut reader, one_hand_ready);
        let record = reader
            .next_record()
            .expect("fault record reads")
            .expect("fixture contains the integrity fault frame");
        let mut frame = decode_frame(&record.payload).expect("fault frame decodes");
        frame.valid_sensor_mask &= !1;
        let terminal =
            LiveTerminalReason::EvidenceInvalidatingAbstention(AbstainReason::MissingSensor);
        assert!(matches!(
            push_live_candidate(&mut live,
                TransportCandidate::Decoded(
                    encode_frame(&frame).expect("missing-sensor frame encodes").to_vec()
                ),
                record.host_time_ns,
            ),
            Err(LiveCalibratedError::TerminalFault(actual)) if actual == terminal
        ));
        assert_eq!(live.report().terminal_reason, Some(terminal.clone()));
        assert_eq!(live.report().wear_session_status(), REVOKED_STATUS);
        assert_eq!(live.report().armed_transitions, 0);
        assert!(!live.report().output_neutrality_confirmed);
        assert!(matches!(
            push_live_candidate(&mut live,
                TransportCandidate::Decoded(record.payload),
                record.host_time_ns,
            ),
            Err(LiveCalibratedError::WearSessionRevoked(actual)) if actual == terminal
        ));
        let execution = live.finish().expect("failed all-up remains inspectable");
        assert_eq!(execution.report.terminal_reason, Some(terminal));
        assert!(execution.report.final_key_state_neutral);
        assert!(!execution.report.output_neutrality_confirmed);
        assert_eq!(execution.report.output_events, 0);
        assert_eq!(
            execution.neutralization_failure,
            Some(RuntimeOutputError {
                event: None,
                cause: LiveOutputError::Neutralization,
                recovery_error: None,
            })
        );
        assert_eq!(execution.output.release_all_calls, 2);
    }
}

#[test]
fn post_arm_partial_window_integrity_faults_revoke_before_output() {
    let fixture = fixture();
    for (mask, flags, skew, reason) in [
        (ALL_SENSORS_VALID & !1, 0, 0, AbstainReason::MissingSensor),
        (
            ALL_SENSORS_VALID,
            FLAG_CLOCK_UNSYNCED,
            0,
            AbstainReason::AcquisitionFault,
        ),
        (
            ALL_SENSORS_VALID,
            FLAG_ACQUISITION_OVERRUN,
            0,
            AbstainReason::AcquisitionFault,
        ),
        (
            ALL_SENSORS_VALID,
            0,
            MAX_INTER_SENSOR_SKEW_US + 1,
            AbstainReason::TimingSkew,
        ),
    ] {
        for hand in [Hand::Left, Hand::Right] {
            let mut live = LiveCalibratedRuntime::new(
                &fixture.calibration_manifest,
                &fixture.runtime_manifest,
                &fixture.artifact,
                confirm_wearables_unmoved(&fixture),
                RecordingOutput::new(),
            )
            .unwrap();
            let armed = warm_live_until_armed(&mut live, &mut fixture.session.reader());
            assert_eq!(live.report().armed_transitions, 1);
            assert_eq!(live.report().output_events, 0);
            let mut frame = match hand {
                Hand::Left => armed.last_left_frame,
                Hand::Right => armed.last_right_frame,
            };
            advance_test_frame(&mut frame);
            let clean = encode_frame(&frame).unwrap().to_vec();
            frame.valid_sensor_mask = mask;
            frame.flags = flags;
            frame.sensors[0].sample_time_offset_us = i16::try_from(skew).unwrap();
            let terminal = LiveTerminalReason::EvidenceInvalidatingAbstention(reason);
            let receipt = armed.last_receipt_time_ns + 5_000_000;
            assert!(
                matches!(
                    push_live_candidate(&mut live, TransportCandidate::Decoded(encode_frame(&frame).unwrap().to_vec()), receipt),
                    Err(LiveCalibratedError::TerminalFault(actual)) if actual == terminal
                ),
                "fault must revoke before filling a window: {hand:?} {reason:?}"
            );
            assert!(matches!(
                push_live_candidate(&mut live, TransportCandidate::Decoded(clean), receipt),
                Err(LiveCalibratedError::WearSessionRevoked(actual)) if actual == terminal
            ));
            let stopped = live.finish().unwrap();
            assert_eq!(stopped.report.terminal_reason, Some(terminal));
            assert_eq!(stopped.report.output_events, 0);
            assert_eq!(stopped.report.key_decisions, 0);
            assert!(!stopped.report.finished_cleanly);
            assert!(stopped.report.output_neutrality_confirmed);
            assert!(stopped.output.events().is_empty());
        }
    }
}

#[test]
fn post_arm_allowed_sensor_skew_with_forward_sample_remains_live() {
    let fixture = fixture();
    for hand in [Hand::Left, Hand::Right] {
        let mut live = LiveCalibratedRuntime::new(
            &fixture.calibration_manifest,
            &fixture.runtime_manifest,
            &fixture.artifact,
            confirm_wearables_unmoved(&fixture),
            RecordingOutput::new(),
        )
        .unwrap();
        let armed = warm_live_until_armed(&mut live, &mut fixture.session.reader());
        let mut frame = match hand {
            Hand::Left => armed.last_left_frame,
            Hand::Right => armed.last_right_frame,
        };
        let aggregate_step_us = u64::try_from(MAX_INTER_SENSOR_SKEW_US).unwrap() + 1;
        frame.sequence = frame.sequence.wrapping_add(1);
        frame.device_time_us += aggregate_step_us;
        frame.sensors[0].sample_time_offset_us = -i16::try_from(MAX_INTER_SENSOR_SKEW_US).unwrap();
        push_live_frame(
            &mut live,
            &frame,
            armed.last_receipt_time_ns + aggregate_step_us * 1_000,
        );
        let execution = live.finish().unwrap();
        assert_eq!(execution.report.terminal_reason, None);
        assert_eq!(execution.report.armed_transitions, 1);
        assert!(execution.report.runtime_faults.is_empty());
        assert!(execution.report.finished_cleanly);
        assert!(execution.report.output_neutrality_confirmed);
    }
}

#[test]
fn live_sequence_gap_revokes_during_warmup_and_partial_window_fill() {
    let fixture = fixture();
    for armed_first in [false, true] {
        let mut live = LiveCalibratedRuntime::new(
            &fixture.calibration_manifest,
            &fixture.runtime_manifest,
            &fixture.artifact,
            confirm_wearables_unmoved(&fixture),
            RecordingOutput::new(),
        )
        .unwrap();
        let (mut frame, receipt) = if armed_first {
            let armed = warm_live_until_armed(&mut live, &mut fixture.session.reader());
            (armed.last_left_frame, armed.last_receipt_time_ns)
        } else {
            let first = fixture.session.reader().next_record().unwrap().unwrap();
            let frame = decode_frame(&first.payload).unwrap();
            push_live_candidate(
                &mut live,
                TransportCandidate::Decoded(first.payload),
                first.host_time_ns,
            )
            .unwrap();
            (frame, first.host_time_ns)
        };
        advance_test_frame(&mut frame);
        frame.sequence = frame.sequence.wrapping_add(1);
        let terminal = LiveTerminalReason::EvidenceInvalidatingAbstention(
            AbstainReason::SequenceDiscontinuity,
        );
        assert!(matches!(
            push_live_candidate(&mut live, TransportCandidate::Decoded(encode_frame(&frame).unwrap().to_vec()), receipt + 5_000_000),
            Err(LiveCalibratedError::TerminalFault(actual)) if actual == terminal
        ));
        let stopped = live.finish().unwrap();
        assert_eq!(stopped.report.terminal_reason, Some(terminal));
        assert_eq!(stopped.report.output_events, 0);
        assert!(stopped.report.output_neutrality_confirmed);
    }
}

#[test]
fn live_sensor_time_regressions_revoke_before_clock_or_window_progress() {
    use invisible_keyboard::{decoder::WindowError, pipeline::PipelineError};
    let fixture = fixture();
    for armed_first in [false, true] {
        let mut live = LiveCalibratedRuntime::new(
            &fixture.calibration_manifest,
            &fixture.runtime_manifest,
            &fixture.artifact,
            confirm_wearables_unmoved(&fixture),
            RecordingOutput::new(),
        )
        .unwrap();
        let (mut frame, last_receipt) = if armed_first {
            let armed = warm_live_until_armed(&mut live, &mut fixture.session.reader());
            (armed.last_left_frame, armed.last_receipt_time_ns)
        } else {
            let mut reader = fixture.session.reader();
            let mut previous = None;
            for _ in 0..4 {
                let record = reader.next_record().unwrap().unwrap();
                previous = Some((decode_frame(&record.payload).unwrap(), record.host_time_ns));
                push_live_candidate(
                    &mut live,
                    TransportCandidate::Decoded(record.payload),
                    record.host_time_ns,
                )
                .unwrap();
            }
            previous.unwrap()
        };
        advance_test_frame(&mut frame);
        let clean = encode_frame(&frame).unwrap().to_vec();
        for sensor in &mut frame.sensors {
            sensor.sample_time_offset_us = sensor.sample_time_offset_us.checked_sub(5_001).unwrap();
        }
        let fault = CalibratedRuntimeFault::Pipeline(PipelineError::InvalidWindow(
            WindowError::SensorTimeRegression { sensor: 0 },
        ));
        let terminal = LiveTerminalReason::RuntimeFault(fault.clone());
        assert!(matches!(
            push_live_candidate(&mut live, TransportCandidate::Decoded(encode_frame(&frame).unwrap().to_vec()), last_receipt + 5_000_000),
            Err(LiveCalibratedError::TerminalFault(actual)) if actual == terminal
        ));
        assert!(matches!(
            push_live_candidate(&mut live, TransportCandidate::Decoded(clean), last_receipt + 5_000_000),
            Err(LiveCalibratedError::WearSessionRevoked(actual)) if actual == terminal
        ));
        let stopped = live.finish().unwrap();
        assert_eq!(stopped.report.terminal_reason, Some(terminal));
        assert_eq!(stopped.report.runtime_faults, vec![fault]);
        assert_eq!(stopped.report.output_events, 0);
        assert!(stopped.report.output_neutrality_confirmed);
    }
}

fn continue_left_until_right_watchdog(
    live: &mut LiveCalibratedRuntime<RecordingOutput>,
    reader: &mut SessionReader<'_>,
    mut last_left: SensorFrame,
) -> (SensorFrame, u64) {
    let mut last_receipt_time_ns = 0;
    while live.report().watchdog_timeouts == 0 {
        let record = reader
            .next_record()
            .expect("record reads")
            .expect("left traffic reaches the right-hand watchdog deadline");
        let frame = decode_frame(&record.payload).expect("session frame decodes");
        if frame.hand == Hand::Right {
            continue;
        }
        last_left = frame;
        last_receipt_time_ns = record.host_time_ns;
        let result = push_live_candidate(
            &mut *live,
            TransportCandidate::Decoded(record.payload),
            record.host_time_ns,
        );
        match result {
            Ok(()) => {}
            Err(LiveCalibratedError::TerminalFault(LiveTerminalReason::WatchdogTimeout {
                ..
            })) => break,
            Err(error) => panic!("unexpected left-stream fault: {error:?}"),
        }
    }
    (last_left, last_receipt_time_ns)
}

fn advance_test_frame(frame: &mut SensorFrame) {
    frame.sequence = frame.sequence.wrapping_add(1);
    frame.device_time_us += 5_000;
}

fn append_replay_sensor_records(writer: &mut SessionWriter<Vec<u8>>, session: &FinalizedSession) {
    let mut reader = session.reader();
    while let Some(record) = reader.next_record().expect("sensor session reads") {
        let frame = record.decode().expect("fixture has sensor records");
        writer
            .append_frame(
                ControllerStream::from(frame.hand),
                &frame,
                record.host_time_ns,
            )
            .expect("sensor evidence writes");
    }
}

#[test]
fn calibrated_replay_rejects_forged_offline_clock_authority_before_output() {
    let fixture = fixture();
    for (field, value) in [
        ("slope_ns_per_us", serde_json::json!(1000.01)),
        ("drift_ppm", serde_json::json!(1.0)),
        ("residual_p95_ns", serde_json::json!(1)),
        (
            "sample_count",
            serde_json::json!(fixture.clocks.epochs[0].fit.report().sample_count + 1),
        ),
    ] {
        let mut json = serde_json::to_value(&fixture.clocks).unwrap();
        json["epochs"][0]["fit"]["report"][field] = value;
        let forged: ClockEpochSet = serde_json::from_value(json).unwrap();
        forged
            .validate()
            .expect("forged report claims passing numeric gates");
        let output = SharedRecordingOutput::default();
        let observed = output.clone();
        assert!(
            run_calibrated_replay(
                &fixture.calibration_manifest,
                &fixture.runtime_manifest,
                &forged,
                &fixture.artifact,
                &fixture.held_out_validation,
                &fixture.session,
                output,
            )
            .is_err(),
            "altered {field} must not grant calibrated replay authority"
        );
        assert_eq!(observed.event_count(), 0);
        assert_eq!(
            observed.release_all_calls(),
            0,
            "admission rejects before opening runtime output"
        );
    }

    for clock_fault in [false, true] {
        let mut writer = SessionWriter::new(Vec::new(), *fixture.session.header()).unwrap();
        let mut reader = fixture.session.reader();
        let mut ordinal = 0_u64;
        while let Some(record) = reader.next_record().unwrap() {
            let mut frame = record.decode().unwrap();
            if clock_fault && ordinal == 40 {
                frame.flags |= invisible_keyboard::protocol::FLAG_CLOCK_UNSYNCED;
            }
            let phase = (ordinal / 2) % 32;
            let delay_ns = if clock_fault {
                0
            } else {
                phase.min(32 - phase) * 1_000_000
            };
            writer
                .append_frame(
                    ControllerStream::from(frame.hand),
                    &frame,
                    record.host_time_ns + delay_ns,
                )
                .unwrap();
            ordinal += 1;
        }
        let session = writer.finish_finalized().unwrap();
        assert!(ClockEpochSet::fit_session(&fixture.runtime_manifest, &session).is_err());
        let output = SharedRecordingOutput::default();
        let observed = output.clone();
        assert!(
            run_calibrated_replay(
                &fixture.calibration_manifest,
                &fixture.runtime_manifest,
                &fixture.clocks,
                &fixture.artifact,
                &fixture.held_out_validation,
                &session,
                output,
            )
            .is_err()
        );
        assert_eq!(observed.event_count(), 0);
        assert_eq!(observed.release_all_calls(), 0);
    }
}

fn replay_pending_overflow_session(fixture: &ReplayFixture) -> FinalizedSession {
    let mut writer =
        SessionWriter::new(Vec::new(), *fixture.session.header()).expect("overflow session starts");
    for _ in 0..=MAX_MERGE_PENDING_RECORDS {
        writer
            .append_candidate(
                ControllerStream::Left,
                SessionRecordKind::Malformed,
                &[0xff],
                1,
                5_000_000_000,
            )
            .expect("malformed evidence writes");
    }
    append_replay_sensor_records(&mut writer, &fixture.session);
    writer
        .finish_finalized()
        .expect("overflow session remains valid raw evidence")
}

#[test]
fn replay_pending_capacity_fails_closed_with_a_typed_error() {
    let fixture = fixture();
    let session = replay_pending_overflow_session(&fixture);
    let output = SharedRecordingOutput::default();
    let observed_output = output.clone();

    assert!(matches!(
        run_calibrated_replay(
            &fixture.calibration_manifest,
            &fixture.runtime_manifest,
            &fixture.clocks,
            &fixture.artifact,
            &fixture.held_out_validation,
            &session,
            output,
        ),
        Err(CalibratedReplayError::PendingCapacityExceeded {
            maximum: MAX_MERGE_PENDING_RECORDS,
            required,
        }) if required == MAX_MERGE_PENDING_RECORDS + 1
    ));
    assert_eq!(observed_output.event_count(), 0);
    assert!(observed_output.release_all_calls() >= 2);
}

#[test]
fn calibrated_replay_rejects_gate0_control_records_instead_of_replaying_them() {
    let fixture = fixture();
    let identity = fixture.runtime_manifest.hands[0]
        .frame_identity()
        .expect("runtime left identity is valid");
    let request = QuiesceRequest {
        hand: Hand::Left,
        controller_id_sha256: identity.controller_id_sha256,
        boot_id: identity.boot_id,
        session_id: SESSION_ID,
        request_start_time_ns: 5_000_000_000,
    };
    let mut writer =
        SessionWriter::new(Vec::new(), *fixture.session.header()).expect("control session starts");
    writer
        .append_quiesce_request(ControllerStream::Left, &request, 5_000_000_000)
        .expect("valid request evidence writes");
    append_replay_sensor_records(&mut writer, &fixture.session);
    let control_session = writer
        .finish_finalized()
        .expect("control session finalizes and audits");
    let output = SharedRecordingOutput::default();
    let observed_output = output.clone();

    assert!(matches!(
        run_calibrated_replay(
            &fixture.calibration_manifest,
            &fixture.runtime_manifest,
            &fixture.clocks,
            &fixture.artifact,
            &fixture.held_out_validation,
            &control_session,
            output,
        ),
        Err(CalibratedReplayError::UnexpectedControlRecord {
            ordinal: 0,
            kind: SessionRecordKind::QuiesceRequest,
        })
    ));
    assert_eq!(observed_output.event_count(), 0);
    assert!(observed_output.release_all_calls() >= 2);
}

#[test]
fn recorded_replay_preserves_the_source_layout_confirmation_receipt() {
    let app = CalibrationApp::new(EvidenceMode::RecordedReplay);
    app.apply_hardware_event(HardwareCalibrationEvent::Connected)
        .expect("recorded controller evidence connects");
    app.apply_hardware_event(HardwareCalibrationEvent::HardwareChecked)
        .expect("recorded controller evidence passes its stored gate");
    let identified = app
        .apply_hardware_event(HardwareCalibrationEvent::KeyboardDeviceIdentified {
            device: "Recorded fixture keyboard (fixture/input0)".to_owned(),
        })
        .expect("recorded keyboard provenance is selected");
    let replay_instruction_revision = identified
        .keyboard_layout_instruction_revision
        .expect("the replay UI also has a current presentation revision");
    let source_receipt = KeyboardLayoutConfirmationReceipt::us_qwerty(777, 987_654);
    assert_ne!(
        replay_instruction_revision, source_receipt.instruction_revision,
        "replay must not replace source provenance with its current UI revision"
    );

    let confirmed = app
        .apply_recorded_keyboard_layout_confirmation(source_receipt.clone())
        .expect("immutable recorded evidence supplies its original receipt");
    assert_eq!(
        confirmed.keyboard_layout_confirmation_revision,
        Some(source_receipt.instruction_revision)
    );
    assert_eq!(
        confirmed.keyboard_layout_confirmation_host_time_ns,
        Some(source_receipt.acknowledged_host_time_ns)
    );
    assert_eq!(
        app.keyboard_layout_confirmation_receipt()
            .expect("replayed receipt remains readable"),
        Some(source_receipt.clone())
    );
    assert_eq!(
        app.keyboard_layout_decision_state()
            .expect("typed replay decision remains readable"),
        KeyboardLayoutDecisionState::Confirmed(source_receipt)
    );
}

#[test]
fn calibrated_live_fails_when_startup_neutralization_fails() {
    let fixture = fixture();
    let output = FaultInjectingLiveOutput {
        fail_startup: true,
        fail_neutralization: false,
        pressed: true,
        release_all_calls: 0,
    };

    assert!(matches!(
        LiveCalibratedRuntime::new(
            &fixture.calibration_manifest,
            &fixture.runtime_manifest,
            &fixture.artifact,
            confirm_wearables_unmoved(&fixture),
            output,
        ),
        Err(LiveCalibratedError::Output(RuntimeOutputError {
            event: None,
            cause: LiveOutputError::Startup,
            recovery_error: None,
        }))
    ));
}

#[test]
fn calibrated_live_output_failure_recovers_and_finishes_neutral() {
    let fixture = fixture();
    let mut live = LiveCalibratedRuntime::new(
        &fixture.calibration_manifest,
        &fixture.runtime_manifest,
        &fixture.artifact,
        confirm_wearables_unmoved(&fixture),
        FaultInjectingLiveOutput {
            fail_startup: false,
            fail_neutralization: false,
            pressed: false,
            release_all_calls: 0,
        },
    )
    .expect("live startup reaches neutral");
    let mut reader = SessionReader::new(&fixture.session);
    let mut observed_failure = false;
    while let Some(record) = reader.next_record().expect("record reads") {
        match push_live_candidate(
            &mut live,
            TransportCandidate::Decoded(record.payload),
            record.host_time_ns,
        ) {
            Ok(()) => {}
            Err(LiveCalibratedError::Output(RuntimeOutputError {
                cause: LiveOutputError::Transition,
                recovery_error: None,
                ..
            })) => {
                observed_failure = true;
                break;
            }
            Err(error) => panic!("unexpected calibrated-live fault: {error:?}"),
        }
    }

    assert!(observed_failure);
    assert_eq!(live.report().wear_session_status(), REVOKED_STATUS);
    assert_eq!(
        live.report().terminal_reason,
        Some(LiveTerminalReason::OutputFailure)
    );
    let execution = live.finish().expect("neutral stopped runtime finishes");
    assert!(execution.report.final_key_state_neutral);
    assert!(execution.report.output_neutrality_confirmed);
    assert_eq!(execution.neutralization_failure, None);
    assert!(!execution.output.pressed);
    assert!(execution.output.release_all_calls >= 2);
}

#[test]
fn malformed_oversized_and_unterminated_transport_terminally_revoke() {
    let cases = [
        (
            TransportCandidate::Malformed {
                encoded: vec![0xff],
            },
            TransportFault::Malformed,
        ),
        (
            TransportCandidate::Oversized {
                prefix: vec![0x01, 0x02],
                actual_len: 999,
            },
            TransportFault::Oversized { actual_len: 999 },
        ),
        (
            TransportCandidate::Unterminated {
                encoded: vec![0x03],
            },
            TransportFault::Unterminated,
        ),
    ];

    for (candidate, expected_fault) in cases {
        let fixture = fixture();
        let mut live = LiveCalibratedRuntime::new(
            &fixture.calibration_manifest,
            &fixture.runtime_manifest,
            &fixture.artifact,
            confirm_wearables_unmoved(&fixture),
            RecordingOutput::new(),
        )
        .expect("live binding succeeds");

        assert!(matches!(
            push_live_candidate(&mut live, candidate, 1_000_000_000),
            Err(LiveCalibratedError::TerminalFault(
            LiveTerminalReason::RuntimeFault(CalibratedRuntimeFault::Transport(actual))
            )) if actual == expected_fault
        ));
        assert_eq!(live.report().invalid_candidates, 1);
        assert_eq!(live.report().wear_session_status(), REVOKED_STATUS);
        assert!(matches!(
            live.disconnect(),
            Err(LiveCalibratedError::WearSessionRevoked(
                LiveTerminalReason::RuntimeFault(CalibratedRuntimeFault::Transport(_))
            ))
        ));
        let execution = live.finish().expect("revoked run remains inspectable");
        assert!(execution.output.events().is_empty());
        assert!(execution.report.final_key_state_neutral);
        assert!(execution.report.output_neutrality_confirmed);
        assert_eq!(execution.neutralization_failure, None);
    }
}

#[test]
fn calibrated_live_preserves_transport_primary_and_reports_failed_all_up() {
    let fixture = fixture();
    let mut live = LiveCalibratedRuntime::new(
        &fixture.calibration_manifest,
        &fixture.runtime_manifest,
        &fixture.artifact,
        confirm_wearables_unmoved(&fixture),
        FaultInjectingLiveOutput {
            fail_startup: false,
            fail_neutralization: true,
            pressed: false,
            release_all_calls: 0,
        },
    )
    .expect("live startup reaches neutral");

    assert!(matches!(
        push_live_candidate(
            &mut live,
            TransportCandidate::Malformed {
                encoded: vec![0xff],
            },
            1_000_000_000,
        ),
        Err(LiveCalibratedError::TerminalFault(
            LiveTerminalReason::RuntimeFault(CalibratedRuntimeFault::Transport(
                TransportFault::Malformed
            ))
        ))
    ));
    assert_eq!(live.report().wear_session_status(), REVOKED_STATUS);
    assert!(!live.report().output_neutrality_confirmed);
    assert!(matches!(
        live.tick(1_000_000_001),
        Err(LiveCalibratedError::WearSessionRevoked(
            LiveTerminalReason::RuntimeFault(CalibratedRuntimeFault::Transport(
                TransportFault::Malformed
            ))
        ))
    ));

    let execution = live.finish().expect("the revoked run remains inspectable");
    assert!(execution.report.final_key_state_neutral);
    assert!(!execution.report.output_neutrality_confirmed);
    assert_eq!(
        execution.neutralization_failure,
        Some(RuntimeOutputError {
            event: None,
            cause: LiveOutputError::Neutralization,
            recovery_error: None,
        })
    );
    assert_eq!(execution.output.release_all_calls, 2);
}

#[test]
fn live_pending_capacity_terminally_revokes_without_rearm() {
    let fixture = fixture();
    let mut live = LiveCalibratedRuntime::new(
        &fixture.calibration_manifest,
        &fixture.runtime_manifest,
        &fixture.artifact,
        confirm_wearables_unmoved(&fixture),
        RecordingOutput::new(),
    )
    .expect("live binding succeeds");
    let mut reader = fixture.session.reader();
    let armed = warm_live_until_armed(&mut live, &mut reader);
    let mut frame = armed.last_left_frame;

    let mut overflow = None;
    for _ in 0..=MAX_MERGE_PENDING_RECORDS {
        frame.sequence = frame.sequence.wrapping_add(1);
        frame.device_time_us += 1;
        let result = push_live_candidate(
            &mut live,
            TransportCandidate::Decoded(
                encode_frame(&frame).expect("burst frame encodes").to_vec(),
            ),
            armed.last_receipt_time_ns,
        );
        match result {
            Ok(()) => {}
            Err(LiveCalibratedError::TerminalFault(reason)) => {
                overflow = Some(reason);
                break;
            }
            Err(error) => panic!("unexpected live burst fault: {error:?}"),
        }
    }

    assert_eq!(
        overflow,
        Some(LiveTerminalReason::PendingCapacityExceeded {
            maximum: MAX_MERGE_PENDING_RECORDS,
            required: MAX_MERGE_PENDING_RECORDS + 1,
        })
    );
    assert_eq!(
        live.report().terminal_reason,
        Some(LiveTerminalReason::PendingCapacityExceeded {
            maximum: MAX_MERGE_PENDING_RECORDS,
            required: MAX_MERGE_PENDING_RECORDS + 1,
        })
    );
    assert_eq!(
        live.report().maximum_pending_frames,
        MAX_MERGE_PENDING_RECORDS
    );
    assert_eq!(live.report().output_events, 0);
    assert!(matches!(
        live.tick(armed.last_receipt_time_ns),
        Err(LiveCalibratedError::WearSessionRevoked(
            LiveTerminalReason::PendingCapacityExceeded {
                maximum: MAX_MERGE_PENDING_RECORDS,
                required,
            }
        )) if required == MAX_MERGE_PENDING_RECORDS + 1
    ));

    let execution = live.finish().expect("revoked run remains inspectable");
    assert!(!execution.report.finished_cleanly);
    assert!(execution.report.final_key_state_neutral);
    assert!(execution.output.events().is_empty());
    assert!(execution.output.release_all_calls() >= 2);
}

#[test]
fn evidence_invalidating_abstention_terminally_revokes() {
    let fixture = fixture();
    let mut live = LiveCalibratedRuntime::new(
        &fixture.calibration_manifest,
        &fixture.runtime_manifest,
        &fixture.artifact,
        confirm_wearables_unmoved(&fixture),
        RecordingOutput::new(),
    )
    .expect("live binding succeeds");
    let mut reader = SessionReader::new(&fixture.session);
    let armed = warm_live_until_armed(&mut live, &mut reader);
    let mut left = armed.last_left_frame;
    let mut right = armed.last_right_frame;
    let mut receipt_time_ns = armed.last_receipt_time_ns;

    // Keep healthy idle frames pending when the acquisition fault arrives.
    for _ in 0..16 {
        advance_test_frame(&mut left);
        advance_test_frame(&mut right);
        receipt_time_ns += 5_000_000;
        push_live_frame(&mut live, &left, receipt_time_ns);
        push_live_frame(&mut live, &right, receipt_time_ns + 1_000_000);
    }

    let mut invalid = left;
    advance_test_frame(&mut invalid);
    invalid.valid_sensor_mask = 0;

    let invalid_receipt_time_ns = receipt_time_ns + 5_000_000;
    let terminal_result = push_live_candidate(
        &mut live,
        TransportCandidate::Decoded(
            encode_frame(&invalid)
                .expect("quality-fault frame encodes")
                .to_vec(),
        ),
        invalid_receipt_time_ns,
    );
    assert!(
        matches!(
            terminal_result,
            Err(LiveCalibratedError::TerminalFault(
                LiveTerminalReason::EvidenceInvalidatingAbstention(
                    invisible_keyboard::decoder::AbstainReason::MissingSensor
                )
            ))
        ),
        "unexpected result {terminal_result:?}; report={:?}",
        live.report()
    );
    assert_eq!(live.report().wear_session_status(), REVOKED_STATUS);
    assert!(live.report().runtime_faults.is_empty());
    assert!(matches!(
        live.tick(invalid_receipt_time_ns + 50_000_001),
        Err(LiveCalibratedError::WearSessionRevoked(
            LiveTerminalReason::EvidenceInvalidatingAbstention(_)
        ))
    ));
    let execution = live.finish().expect("revoked run remains inspectable");
    assert!(execution.left_clock_fit.is_none());
    assert!(execution.right_clock_fit.is_none());
    assert!(execution.report.final_key_state_neutral);
}

#[test]
fn stale_device_backlog_then_fresh_timestamp_jump_never_arms_or_emits() {
    let fixture = fixture();
    let mut live = LiveCalibratedRuntime::new(
        &fixture.calibration_manifest,
        &fixture.runtime_manifest,
        &fixture.artifact,
        confirm_wearables_unmoved(&fixture),
        RecordingOutput::new(),
    )
    .expect("live binding succeeds");
    let mut reader = fixture.session.reader();
    let mut frame = loop {
        let record = reader
            .next_record()
            .expect("record reads")
            .expect("left template exists");
        let frame = record.decode().expect("template frame decodes");
        if frame.hand == Hand::Left {
            break frame;
        }
    };

    // Three buffered device frames arrive with plausible adjacent host
    // receipts. The next frame is current device time after a ten-minute gap,
    // but arrives only 5 ms later at the host.
    for index in 0..3_u32 {
        frame.sequence = index;
        frame.device_time_us = u64::from(index) * 5_000;
        push_live_candidate(
            &mut live,
            TransportCandidate::Decoded(
                encode_frame(&frame).expect("stale frame encodes").to_vec(),
            ),
            5_000_000_000 + u64::from(index) * 5_000_000,
        )
        .expect("short stale prefix stays disarmed");
    }
    assert_eq!(live.report().armed_transitions, 0);

    frame.sequence = 3;
    frame.device_time_us = 600_000_000;
    assert!(matches!(
        push_live_candidate(
            &mut live,
            TransportCandidate::Decoded(
                encode_frame(&frame).expect("fresh frame encodes").to_vec(),
            ),
            5_015_000_000,
        ),
        Err(LiveCalibratedError::Clock {
            hand: Hand::Left,
            ..
        })
    ));
    assert_eq!(live.report().armed_transitions, 0);
    assert_eq!(live.report().wear_session_status(), REVOKED_STATUS);
    assert_eq!(
        live.report().terminal_reason,
        Some(LiveTerminalReason::ClockFailure(Hand::Left))
    );
    assert!(matches!(
        live.tick(5_020_000_000),
        Err(LiveCalibratedError::WearSessionRevoked(
            LiveTerminalReason::ClockFailure(Hand::Left)
        ))
    ));
    let execution = live.finish().expect("revoked run remains inspectable");
    assert!(execution.output.events().is_empty());
    assert!(execution.report.final_key_state_neutral);
}

#[test]
fn host_receipt_regression_revokes_before_clock_or_output_progress() {
    let fixture = fixture();
    let mut live = LiveCalibratedRuntime::new(
        &fixture.calibration_manifest,
        &fixture.runtime_manifest,
        &fixture.artifact,
        confirm_wearables_unmoved(&fixture),
        RecordingOutput::new(),
    )
    .expect("live binding succeeds");
    let mut reader = fixture.session.reader();
    let first = reader
        .next_record()
        .expect("record reads")
        .expect("first record exists");
    push_live_candidate(
        &mut live,
        TransportCandidate::Decoded(first.payload.clone()),
        first.host_time_ns,
    )
    .expect("first receipt establishes host order");

    assert!(matches!(
        push_live_candidate_at(
            &mut live,
            TransportCandidate::Decoded(
                encode_frame(&{
                    let mut next = decode_frame(&first.payload).unwrap();
                    advance_test_frame(&mut next);
                    next
                })
                .unwrap()
                .to_vec()
            ),
            first.host_time_ns - 1,
            first.host_time_ns,
        ),
        Err(LiveCalibratedError::HostReceiptRegression { ordinal: 1 })
    ));
    assert_eq!(live.report().total_candidates, 1);
    assert_eq!(
        live.report().terminal_reason,
        Some(LiveTerminalReason::HostReceiptRegression { ordinal: 1 })
    );
    assert!(matches!(
        live.disconnect(),
        Err(LiveCalibratedError::WearSessionRevoked(
            LiveTerminalReason::HostReceiptRegression { ordinal: 1 }
        ))
    ));
    let execution = live.finish().expect("revoked run remains inspectable");
    assert!(execution.output.events().is_empty());
    assert!(execution.left_clock_fit.is_none());
    assert!(execution.right_clock_fit.is_none());
    assert!(execution.report.final_key_state_neutral);
}

#[test]
fn host_receipt_regression_remains_primary_when_independent_all_up_fails() {
    let fixture = fixture();
    let mut live = LiveCalibratedRuntime::new(
        &fixture.calibration_manifest,
        &fixture.runtime_manifest,
        &fixture.artifact,
        confirm_wearables_unmoved(&fixture),
        FaultInjectingLiveOutput {
            fail_startup: false,
            fail_neutralization: true,
            pressed: false,
            release_all_calls: 0,
        },
    )
    .expect("live startup reaches neutral");
    let mut reader = fixture.session.reader();
    let first = reader
        .next_record()
        .expect("record reads")
        .expect("first record exists");
    push_live_candidate(
        &mut live,
        TransportCandidate::Decoded(first.payload.clone()),
        first.host_time_ns,
    )
    .expect("first receipt establishes host order");

    assert!(matches!(
        push_live_candidate_at(
            &mut live,
            TransportCandidate::Decoded(
                encode_frame(&{
                    let mut next = decode_frame(&first.payload).unwrap();
                    advance_test_frame(&mut next);
                    next
                })
                .unwrap()
                .to_vec()
            ),
            first.host_time_ns - 1,
            first.host_time_ns,
        ),
        Err(LiveCalibratedError::HostReceiptRegression { ordinal: 1 })
    ));
    assert_eq!(
        live.report().terminal_reason,
        Some(LiveTerminalReason::HostReceiptRegression { ordinal: 1 })
    );
    assert!(!live.report().output_neutrality_confirmed);
    assert!(matches!(
        live.disconnect(),
        Err(LiveCalibratedError::WearSessionRevoked(
            LiveTerminalReason::HostReceiptRegression { ordinal: 1 }
        ))
    ));

    let execution = live.finish().expect("the revoked run remains inspectable");
    assert!(execution.report.final_key_state_neutral);
    assert!(!execution.report.output_neutrality_confirmed);
    assert_eq!(
        execution.neutralization_failure,
        Some(RuntimeOutputError {
            event: None,
            cause: LiveOutputError::Neutralization,
            recovery_error: None,
        })
    );
}

#[test]
fn artifact_bound_two_hand_replay_emits_exact_core28_taps_and_neutralizes() {
    let fixture = fixture();

    let execution = run_calibrated_replay(
        &fixture.calibration_manifest,
        &fixture.runtime_manifest,
        &fixture.clocks,
        &fixture.artifact,
        &fixture.held_out_validation,
        &fixture.session,
        RecordingOutput::new(),
    )
    .expect("bound replay succeeds");

    assert_eq!(execution.report.evidence_label(), "replay-tested");
    assert!(execution.report.limitation().contains("does not establish"));
    assert_eq!(execution.report.left_frames, 80);
    assert_eq!(execution.report.right_frames, 80);
    assert_eq!(execution.report.key_decisions, 2);
    assert_eq!(execution.report.output_events, 4);
    assert!(execution.report.runtime_faults.is_empty());
    assert!(execution.report.final_key_state_neutral);
    assert_eq!(execution.output.release_all_calls(), 2);
    assert_eq!(execution.report.fixed_post_trigger_delay_us, 15_000);
    assert_eq!(
        execution
            .output
            .events()
            .iter()
            .step_by(2)
            .map(|event| event.usage)
            .collect::<Vec<_>>(),
        [KeyUsage::A, KeyUsage::H]
    );
}

#[test]
fn calibrated_replay_preserves_output_cause_and_independent_recovery_failure() {
    let fixture = fixture();

    assert!(matches!(
        run_calibrated_replay(
            &fixture.calibration_manifest,
            &fixture.runtime_manifest,
            &fixture.clocks,
            &fixture.artifact,
            &fixture.held_out_validation,
            &fixture.session,
            FaultInjectingLiveOutput {
                fail_startup: false,
                fail_neutralization: true,
                pressed: false,
                release_all_calls: 0,
            },
        ),
        Err(CalibratedReplayError::Output(RuntimeOutputError {
            event: Some(KeyEvent {
                state: EventState::Up,
                ..
            }),
            cause: LiveOutputError::Transition,
            recovery_error: Some(LiveOutputError::Neutralization),
        }))
    ));
}

#[test]
fn nonidentity_finger_and_dorsal_axes_decode_identically_in_replay_and_live() {
    let fixture = fixture_with_axis_rotation(true);

    let replay = run_calibrated_replay(
        &fixture.calibration_manifest,
        &fixture.runtime_manifest,
        &fixture.clocks,
        &fixture.artifact,
        &fixture.held_out_validation,
        &fixture.session,
        RecordingOutput::new(),
    )
    .expect("rotated raw replay normalizes before decoding");
    assert_eq!(
        replay
            .output
            .events()
            .iter()
            .step_by(2)
            .map(|event| event.usage)
            .collect::<Vec<_>>(),
        [KeyUsage::A, KeyUsage::H]
    );

    let mut live = LiveCalibratedRuntime::new(
        &fixture.calibration_manifest,
        &fixture.runtime_manifest,
        &fixture.artifact,
        confirm_wearables_unmoved(&fixture),
        RecordingOutput::new(),
    )
    .expect("rotated live binding succeeds");
    let mut reader = fixture.session.reader();
    let mut last_receipt = 0_u64;
    while let Some(record) = reader.next_record().expect("record reads") {
        last_receipt = record.host_time_ns;
        push_live_candidate(
            &mut live,
            TransportCandidate::Decoded(record.payload),
            record.host_time_ns,
        )
        .expect("rotated live candidate normalizes before decoding");
    }
    live.tick(last_receipt + 20_000_001)
        .expect("merge watermark advances");
    let live = live.finish().expect("rotated live run finishes");
    assert_eq!(
        live.output
            .events()
            .iter()
            .step_by(2)
            .map(|event| event.usage)
            .collect::<Vec<_>>(),
        [KeyUsage::A, KeyUsage::H]
    );
}

#[test]
fn artifact_reference_and_calibration_sensitive_settings_fail_closed() {
    let fixture = fixture();
    let mut wrong_reference = fixture.runtime_manifest.clone();
    wrong_reference.calibration_artifact_sha256 = Some(format_sha256(&[0xee; 32]));
    assert!(matches!(
        run_calibrated_replay(
            &fixture.calibration_manifest,
            &wrong_reference,
            &fixture.clocks,
            &fixture.artifact,
            &fixture.held_out_validation,
            &fixture.session,
            RecordingOutput::new(),
        ),
        Err(CalibratedReplayError::<Infallible>::Binding(
            RuntimeBindingError::ArtifactReferenceMismatch
        ))
    ));

    // The current schema admits one acquisition profile, so a range change is
    // rejected as an invalid manifest before cross-manifest binding checks.
    let mut changed_range = fixture.runtime_manifest.clone();
    changed_range.accelerometer_range_g = 8;
    assert!(matches!(
        run_calibrated_replay(
            &fixture.calibration_manifest,
            &changed_range,
            &fixture.clocks,
            &fixture.artifact,
            &fixture.held_out_validation,
            &fixture.session,
            RecordingOutput::new(),
        ),
        Err(CalibratedReplayError::<Infallible>::Binding(
            RuntimeBindingError::Manifest(ManifestError::InvalidSetting("accelerometer_range_g"))
        ))
    ));

    let mut remounted = fixture.runtime_manifest.clone();
    remounted.hands[0].sensors[0].mount_id = "left-thumb-remounted-v2".to_owned();
    assert!(matches!(
        run_calibrated_replay(
            &fixture.calibration_manifest,
            &remounted,
            &fixture.clocks,
            &fixture.artifact,
            &fixture.held_out_validation,
            &fixture.session,
            RecordingOutput::new(),
        ),
        Err(CalibratedReplayError::<Infallible>::Binding(
            RuntimeBindingError::SensorProvenanceMismatch {
                hand: Hand::Left,
                sensor_index: 0,
            }
        ))
    ));
}

#[test]
fn every_per_sensor_physical_provenance_change_fails_closed() {
    let fixture = fixture();
    let mut changed_manifests = Vec::new();

    let mut changed_mount = fixture.runtime_manifest.clone();
    changed_mount.hands[0].sensors[0].mount_id = "left-thumb-remount-v2".to_owned();
    changed_manifests.push(changed_mount);

    let mut changed_wiring = fixture.runtime_manifest.clone();
    changed_wiring.hands[0].sensors[0].wiring_id = "left-thumb-wiring-v2".to_owned();
    changed_manifests.push(changed_wiring);

    let mut changed_interrupt = fixture.runtime_manifest.clone();
    changed_interrupt.hands[0].sensors[0].interrupt_pin = "left-int6".to_owned();
    changed_manifests.push(changed_interrupt);

    let mut changed_mux = fixture.runtime_manifest.clone();
    changed_mux.hands[0].sensors[0].mux_channel = 6;
    changed_manifests.push(changed_mux);

    let mut changed_axis_map = fixture.runtime_manifest.clone();
    changed_axis_map.hands[0].sensors[0].axis_map = [0, -1, 0, 1, 0, 0, 0, 0, 1];
    changed_manifests.push(changed_axis_map);

    for changed in changed_manifests {
        assert_eq!(
            verify_runtime_binding(
                &fixture.calibration_manifest,
                &changed,
                &fixture.artifact,
                &fixture.held_out_validation,
            ),
            Err(RuntimeBindingError::SensorProvenanceMismatch {
                hand: Hand::Left,
                sensor_index: 0,
            })
        );
    }

    let mut changed_role = fixture.runtime_manifest.clone();
    changed_role.hands[0].sensors.swap(0, 1);
    assert!(matches!(
        verify_runtime_binding(
            &fixture.calibration_manifest,
            &changed_role,
            &fixture.artifact,
            &fixture.held_out_validation,
        ),
        Err(RuntimeBindingError::Manifest(_))
    ));

    let mut changed_artifact = fixture.artifact.clone();
    changed_artifact.sensor_bindings[0].sensor.mount_id =
        "left-thumb-artifact-remount-v2".to_owned();
    let mut runtime_referencing_changed_artifact = fixture.runtime_manifest.clone();
    let changed_validation =
        build_held_out_validation(&fixture.calibration_manifest, &changed_artifact);
    runtime_referencing_changed_artifact.calibration_artifact_sha256 = Some(format_sha256(
        &changed_artifact
            .fingerprint()
            .expect("changed artifact hashes"),
    ));
    runtime_referencing_changed_artifact.calibration_validation_sha256 = Some(format_sha256(
        &changed_validation.validation_report_sha256(),
    ));
    assert_eq!(
        verify_runtime_binding(
            &fixture.calibration_manifest,
            &runtime_referencing_changed_artifact,
            &changed_artifact,
            &changed_validation,
        ),
        Err(RuntimeBindingError::SensorProvenanceMismatch {
            hand: Hand::Left,
            sensor_index: 0,
        })
    );
}

#[test]
fn runtime_artifact_reference_binds_the_frozen_matching_rule() {
    let fixture = fixture();
    let mut changed_artifact = fixture.artifact.clone();
    let mut changed_rule = calibration_matching_rule(&fixture.calibration_manifest);
    changed_rule.neutral_source_sha256 = format_sha256(&[0x05; 32]);
    changed_artifact.calibration_matching_rule_sha256 = format_sha256(
        &changed_rule
            .fingerprint()
            .expect("changed matching rule hashes"),
    );
    let changed_validation = build_held_out_validation_with_rule(
        &fixture.calibration_manifest,
        &changed_artifact,
        changed_rule,
    );

    assert_eq!(
        verify_runtime_binding(
            &fixture.calibration_manifest,
            &fixture.runtime_manifest,
            &changed_artifact,
            &changed_validation,
        ),
        Err(RuntimeBindingError::ArtifactReferenceMismatch)
    );
}

#[test]
fn runtime_binding_allows_only_new_boot_and_session_metadata() {
    let fixture = fixture();
    verify_runtime_binding(
        &fixture.calibration_manifest,
        &fixture.runtime_manifest,
        &fixture.artifact,
        &fixture.held_out_validation,
    )
    .expect("boot identity, study, split, and artifact reference may change per session");

    for changed in [
        {
            let mut changed = fixture.runtime_manifest.clone();
            changed.host_build = "different-host-build".to_owned();
            changed
        },
        {
            let mut changed = fixture.runtime_manifest.clone();
            changed.prompt_corpus_sha256 = format_sha256(&[0xaa; 32]);
            changed
        },
        {
            let mut changed = fixture.runtime_manifest.clone();
            changed.participant_alias = "different-participant".to_owned();
            changed
        },
    ] {
        assert_eq!(
            verify_runtime_binding(
                &fixture.calibration_manifest,
                &changed,
                &fixture.artifact,
                &fixture.held_out_validation,
            ),
            Err(RuntimeBindingError::AcquisitionSettingMismatch)
        );
    }
}

#[test]
fn causal_live_path_warms_both_clocks_before_emitting_and_finishes_neutral() {
    let fixture = fixture();
    let mut live = LiveCalibratedRuntime::new(
        &fixture.calibration_manifest,
        &fixture.runtime_manifest,
        &fixture.artifact,
        confirm_wearables_unmoved(&fixture),
        RecordingOutput::new(),
    )
    .expect("live binding succeeds");
    assert_eq!(
        live.report().wear_session_status(),
        USER_ATTESTED_UNMOVED_STATUS
    );
    let mut reader = SessionReader::new(&fixture.session);
    let mut last_receipt = 0_u64;
    while let Some(record) = reader.next_record().expect("record reads") {
        assert_eq!(record.kind, SessionRecordKind::Decoded);
        last_receipt = record.host_time_ns;
        push_live_candidate(
            &mut live,
            TransportCandidate::Decoded(record.payload),
            record.host_time_ns,
        )
        .expect("causal candidate succeeds");
    }
    live.tick(last_receipt + 20_000_001)
        .expect("merge watermark advances");
    let execution = live.finish().expect("planned shutdown succeeds");

    assert!(execution.report.clock_warmup_frames >= 16);
    assert_eq!(execution.report.armed_transitions, 1);
    assert_eq!(execution.report.key_decisions, 2);
    assert_eq!(execution.report.output_events, 4);
    assert!(execution.report.runtime_faults.is_empty());
    assert!(execution.report.final_key_state_neutral);
    assert!(execution.left_clock_fit.is_some());
    assert!(execution.right_clock_fit.is_some());
    assert_eq!(
        execution
            .output
            .events()
            .iter()
            .step_by(2)
            .map(|event| event.usage)
            .collect::<Vec<_>>(),
        [KeyUsage::A, KeyUsage::H]
    );
}

#[test]
fn clean_finish_revokes_wear_session_without_recording_a_terminal_fault() {
    let fixture = fixture();
    let live = LiveCalibratedRuntime::new(
        &fixture.calibration_manifest,
        &fixture.runtime_manifest,
        &fixture.artifact,
        confirm_wearables_unmoved(&fixture),
        RecordingOutput::new(),
    )
    .expect("live binding succeeds");

    assert_eq!(
        live.report().wear_session_status(),
        USER_ATTESTED_UNMOVED_STATUS
    );
    let execution = live.finish().expect("clean finish succeeds");

    assert!(execution.report.finished_cleanly);
    assert_eq!(execution.report.terminal_reason, None);
    assert_eq!(execution.report.wear_session_status(), REVOKED_STATUS);
    assert!(execution.report.final_key_state_neutral);
}

#[test]
fn live_rejects_an_unacceptable_mapping_before_it_can_arm_output() {
    let fixture = fixture();
    let mut live = LiveCalibratedRuntime::new(
        &fixture.calibration_manifest,
        &fixture.runtime_manifest,
        &fixture.artifact,
        confirm_wearables_unmoved(&fixture),
        RecordingOutput::new(),
    )
    .expect("live binding succeeds");
    let mut reader = fixture.session.reader();
    let mut left_template = None;
    let mut right_template = None;
    while left_template.is_none() || right_template.is_none() {
        let record = reader
            .next_record()
            .expect("record reads")
            .expect("both hand templates exist");
        let frame = record.decode().expect("template frame decodes");
        match frame.hand {
            Hand::Left => left_template.get_or_insert(frame),
            Hand::Right => right_template.get_or_insert(frame),
        };
    }
    let mut left = left_template.expect("left template exists");
    let mut right = right_template.expect("right template exists");

    for index in 0..=20_u32 {
        left.sequence = index;
        left.device_time_us = u64::from(index) * 5_000;
        push_live_frame(
            &mut live,
            &left,
            1_000_000_000 + left.device_time_us * 1_000,
        );
    }
    for index in 0..=20_u32 {
        right.sequence = index;
        right.device_time_us = u64::from(index) * 5_000;
        push_live_frame(
            &mut live,
            &right,
            1_101_000_000 + right.device_time_us * 1_000,
        );
    }
    assert_eq!(live.report().armed_transitions, 0);

    left.sequence = 21;
    left.device_time_us = 105_000;
    let error = push_live_candidate(
        &mut live,
        TransportCandidate::Decoded(
            encode_frame(&left)
                .expect("delayed left frame encodes")
                .to_vec(),
        ),
        1_231_000_000,
    )
    .expect_err("a 126 ms receipt discrepancy fails closed");
    assert!(matches!(
        error,
        LiveCalibratedError::MappedReceiptDeltaTooLarge { .. }
    ));
    assert_eq!(live.report().armed_transitions, 0);
    assert_eq!(live.report().wear_session_status(), REVOKED_STATUS);
    assert!(matches!(
        live.tick(1_232_000_000),
        Err(LiveCalibratedError::WearSessionRevoked(
            LiveTerminalReason::MappedReceiptDeltaTooLarge { .. }
        ))
    ));

    let execution = live.finish().expect("revoked run remains inspectable");
    assert!(execution.left_clock_fit.is_none());
    assert!(execution.right_clock_fit.is_none());
    assert!(execution.output.events().is_empty());
    assert!(execution.report.final_key_state_neutral);
}

#[test]
fn live_watchdog_terminally_revokes_and_cannot_rearm() {
    let fixture = fixture();
    let mut live = LiveCalibratedRuntime::new(
        &fixture.calibration_manifest,
        &fixture.runtime_manifest,
        &fixture.artifact,
        confirm_wearables_unmoved(&fixture),
        RecordingOutput::new(),
    )
    .expect("live binding succeeds");
    let mut reader = SessionReader::new(&fixture.session);
    let armed = warm_live_until_armed(&mut live, &mut reader);

    assert!(matches!(
        live.tick(armed.last_receipt_time_ns + LIVE_WATCHDOG_NS + 1),
        Err(LiveCalibratedError::TerminalFault(
            LiveTerminalReason::WatchdogTimeout {
                left_expired: true,
                right_expired: true,
            }
        ))
    ));

    assert_eq!(live.report().watchdog_timeouts, 1);
    assert_eq!(live.report().left_watchdog_timeouts, 1);
    assert_eq!(live.report().right_watchdog_timeouts, 1);
    assert_eq!(live.report().wear_session_status(), REVOKED_STATUS);
    assert!(matches!(
        live.source_lost(Hand::Left),
        Err(LiveCalibratedError::WearSessionRevoked(
            LiveTerminalReason::WatchdogTimeout { .. }
        ))
    ));
    let execution = live.finish().expect("revoked runtime finishes");
    assert!(!execution.report.finished_cleanly);
    assert!(matches!(
        execution.report.terminal_reason,
        Some(LiveTerminalReason::WatchdogTimeout {
            left_expired: true,
            right_expired: true,
        })
    ));
    assert!(execution.report.final_key_state_neutral);
    assert!(execution.left_clock_fit.is_none());
    assert!(execution.right_clock_fit.is_none());
    assert!(execution.output.events().is_empty());
}

#[test]
fn live_watchdog_revokes_when_right_stops_during_continuous_left_traffic() {
    let fixture = fixture();
    let mut live = LiveCalibratedRuntime::new(
        &fixture.calibration_manifest,
        &fixture.runtime_manifest,
        &fixture.artifact,
        confirm_wearables_unmoved(&fixture),
        RecordingOutput::new(),
    )
    .expect("live binding succeeds");
    let mut reader = SessionReader::new(&fixture.session);
    let armed = warm_live_until_armed(&mut live, &mut reader);

    continue_left_until_right_watchdog(&mut live, &mut reader, armed.last_left_frame);

    assert_eq!(live.report().watchdog_timeouts, 1);
    assert_eq!(live.report().left_watchdog_timeouts, 0);
    assert_eq!(live.report().right_watchdog_timeouts, 1);
    assert_eq!(live.report().armed_transitions, 1);
    assert_eq!(live.report().wear_session_status(), REVOKED_STATUS);
    let execution = live.finish().expect("revoked runtime finishes");
    assert!(execution.report.final_key_state_neutral);
    assert!(execution.left_clock_fit.is_none());
    assert!(execution.right_clock_fit.is_none());
    assert!(execution.output.events().is_empty());
}

#[test]
fn live_watchdog_rejects_every_later_frame_without_new_confirmation() {
    let fixture = fixture();
    let mut live = LiveCalibratedRuntime::new(
        &fixture.calibration_manifest,
        &fixture.runtime_manifest,
        &fixture.artifact,
        confirm_wearables_unmoved(&fixture),
        RecordingOutput::new(),
    )
    .expect("live binding succeeds");
    let mut reader = SessionReader::new(&fixture.session);
    let armed = warm_live_until_armed(&mut live, &mut reader);
    let (mut left_frame, mut left_receipt_time_ns) =
        continue_left_until_right_watchdog(&mut live, &mut reader, armed.last_left_frame);

    advance_test_frame(&mut left_frame);
    left_receipt_time_ns += 5_000_000;
    assert!(matches!(
        push_live_candidate(
            &mut live,
            TransportCandidate::Decoded(
                encode_frame(&left_frame)
                    .expect("later frame encodes")
                    .to_vec(),
            ),
            left_receipt_time_ns,
        ),
        Err(LiveCalibratedError::WearSessionRevoked(
            LiveTerminalReason::WatchdogTimeout { .. }
        ))
    ));
    assert_eq!(live.report().armed_transitions, 1);
    let execution = live.finish().expect("revoked runtime finishes");
    assert!(execution.report.final_key_state_neutral);
    assert!(execution.left_clock_fit.is_none());
    assert!(execution.right_clock_fit.is_none());
    assert!(execution.output.events().is_empty());
}

#[test]
fn live_host_clock_regression_revokes_without_watchdog_attribution() {
    let fixture = fixture();
    let mut live = LiveCalibratedRuntime::new(
        &fixture.calibration_manifest,
        &fixture.runtime_manifest,
        &fixture.artifact,
        confirm_wearables_unmoved(&fixture),
        RecordingOutput::new(),
    )
    .expect("live binding succeeds");
    let mut reader = SessionReader::new(&fixture.session);
    let last_receipt_time_ns = warm_live_until_armed(&mut live, &mut reader).last_receipt_time_ns;

    live.tick(last_receipt_time_ns + 1)
        .expect("forward host tick succeeds");
    assert!(matches!(
        live.tick(last_receipt_time_ns),
        Err(LiveCalibratedError::HostClockRegression)
    ));
    assert_eq!(live.report().watchdog_timeouts, 0);
    assert_eq!(live.report().left_watchdog_timeouts, 0);
    assert_eq!(live.report().right_watchdog_timeouts, 0);

    let execution = live.finish().expect("revoked runtime finishes");
    assert!(execution.report.final_key_state_neutral);
    assert!(execution.left_clock_fit.is_none());
    assert!(execution.right_clock_fit.is_none());
    assert!(execution.output.events().is_empty());
}

#[test]
fn live_accepts_reader_receipts_older_than_a_later_processing_tick() {
    let fixture = fixture();
    let mut live = LiveCalibratedRuntime::new(
        &fixture.calibration_manifest,
        &fixture.runtime_manifest,
        &fixture.artifact,
        confirm_wearables_unmoved(&fixture),
        RecordingOutput::new(),
    )
    .expect("live binding succeeds");
    let mut reader = SessionReader::new(&fixture.session);
    let first = reader
        .next_record()
        .expect("record reads")
        .expect("first candidate exists");

    // The main thread can sample a newer processing-time tick while a reader
    // that already timestamped this chunk is delayed before channel publish.
    live.tick(first.host_time_ns + 30_000_000)
        .expect("newer processing tick succeeds");
    push_live_candidate_at(
        &mut live,
        TransportCandidate::Decoded(first.payload),
        first.host_time_ns,
        first.host_time_ns + 30_000_000,
    )
    .expect("the ordered receipt remains valid after the later tick");

    let execution = live.finish().expect("planned shutdown succeeds");
    assert!(execution.report.final_key_state_neutral);
}

#[test]
fn live_disconnect_discards_pending_work_resets_clocks_and_finishes_neutral() {
    let fixture = fixture();
    let mut live = LiveCalibratedRuntime::new(
        &fixture.calibration_manifest,
        &fixture.runtime_manifest,
        &fixture.artifact,
        confirm_wearables_unmoved(&fixture),
        RecordingOutput::new(),
    )
    .expect("live binding succeeds");
    let mut reader = SessionReader::new(&fixture.session);
    warm_live_until_armed(&mut live, &mut reader);

    live.disconnect().expect("disconnect recovery succeeds");
    assert_eq!(live.report().watchdog_timeouts, 0);
    assert_eq!(live.report().left_watchdog_timeouts, 0);
    assert_eq!(live.report().right_watchdog_timeouts, 0);
    assert_eq!(live.report().wear_session_status(), REVOKED_STATUS);
    assert!(matches!(
        live.tick(u64::MAX),
        Err(LiveCalibratedError::WearSessionRevoked(
            LiveTerminalReason::Disconnected
        ))
    ));
    let execution = live.finish().expect("revoked runtime finishes");

    assert!(execution.report.final_key_state_neutral);
    assert!(execution.left_clock_fit.is_none());
    assert!(execution.right_clock_fit.is_none());
    assert!(execution.output.events().is_empty());
    assert!(execution.output.release_all_calls() >= 2);
}

#[test]
fn live_source_loss_revokes_the_instance_without_rearm() {
    let fixture = fixture();
    let mut live = LiveCalibratedRuntime::new(
        &fixture.calibration_manifest,
        &fixture.runtime_manifest,
        &fixture.artifact,
        confirm_wearables_unmoved(&fixture),
        RecordingOutput::new(),
    )
    .expect("live binding succeeds");
    let mut reader = SessionReader::new(&fixture.session);
    warm_live_until_armed(&mut live, &mut reader);

    live.source_lost(Hand::Left)
        .expect("left source loss forces neutral");
    assert_eq!(live.report().wear_session_status(), REVOKED_STATUS);
    assert!(matches!(
        live.source_lost(Hand::Right),
        Err(LiveCalibratedError::WearSessionRevoked(
            LiveTerminalReason::SourceLost(Hand::Left)
        ))
    ));
    let execution = live.finish().expect("finish after source loss succeeds");

    assert_eq!(execution.report.armed_transitions, 1);
    assert!(execution.report.final_key_state_neutral);
    assert!(execution.left_clock_fit.is_none());
    assert!(execution.right_clock_fit.is_none());
    assert!(execution.output.events().is_empty());
    assert_eq!(execution.output.release_all_calls(), 2);
}

#[test]
fn live_planned_signals_stop_output_through_the_canonical_safety_owner() {
    let fixture = fixture();

    for signal in [ShutdownSignal::Interrupt, ShutdownSignal::Terminate] {
        let mut live = LiveCalibratedRuntime::new(
            &fixture.calibration_manifest,
            &fixture.runtime_manifest,
            &fixture.artifact,
            confirm_wearables_unmoved(&fixture),
            RecordingOutput::new(),
        )
        .expect("live binding succeeds");
        let mut reader = SessionReader::new(&fixture.session);
        warm_live_until_armed(&mut live, &mut reader);

        live.shutdown(signal)
            .expect("planned signal forces neutral");
        assert_eq!(live.report().wear_session_status(), REVOKED_STATUS);
        assert_eq!(
            live.report().terminal_reason,
            Some(LiveTerminalReason::PlannedShutdown(signal))
        );
        let execution = live.finish().expect("finish after signal succeeds");

        assert_eq!(execution.report.armed_transitions, 1);
        assert!(execution.report.final_key_state_neutral);
        assert!(execution.output.events().is_empty());
        assert_eq!(execution.output.release_all_calls(), 2);
    }
}

#[test]
fn live_disconnect_prevents_finish_from_emitting_a_buffered_tap() {
    let fixture = fixture();
    let mut control = LiveCalibratedRuntime::new(
        &fixture.calibration_manifest,
        &fixture.runtime_manifest,
        &fixture.artifact,
        confirm_wearables_unmoved(&fixture),
        RecordingOutput::new(),
    )
    .expect("control live binding succeeds");
    let mut disconnected = LiveCalibratedRuntime::new(
        &fixture.calibration_manifest,
        &fixture.runtime_manifest,
        &fixture.artifact,
        confirm_wearables_unmoved(&fixture),
        RecordingOutput::new(),
    )
    .expect("disconnect live binding succeeds");
    let mut reader = SessionReader::new(&fixture.session);
    let mut left_frames = 0;
    let mut right_frames = 0;

    // Stop with the delayed tap decision inside the merge backlog. Neither a
    // deliberate finish nor a disconnect may turn queued input into output.
    while left_frames < 67 || right_frames < 67 {
        let record = reader
            .next_record()
            .expect("record reads")
            .expect("tap prefix exists");
        let frame = decode_frame(&record.payload).expect("session frame decodes");
        match frame.hand {
            Hand::Left => left_frames += 1,
            Hand::Right => right_frames += 1,
        }
        push_live_candidate(
            &mut control,
            TransportCandidate::Decoded(record.payload.clone()),
            record.host_time_ns,
        )
        .expect("control candidate succeeds");
        push_live_candidate(
            &mut disconnected,
            TransportCandidate::Decoded(record.payload),
            record.host_time_ns,
        )
        .expect("disconnect candidate succeeds");
    }
    assert_eq!(control.report().key_decisions, 0);
    assert_eq!(disconnected.report().key_decisions, 0);

    let control_execution = control.finish().expect("control finish succeeds");
    assert_eq!(control_execution.report.key_decisions, 0);
    assert!(control_execution.output.events().is_empty());

    disconnected
        .disconnect()
        .expect("transport disconnect succeeds");
    let disconnected_execution = disconnected
        .finish()
        .expect("finish after disconnect succeeds");
    assert_eq!(disconnected_execution.report.key_decisions, 0);
    assert!(disconnected_execution.output.events().is_empty());
    assert!(disconnected_execution.report.final_key_state_neutral);
}

fn append_live_ingress_evidence(
    writer: &mut SessionWriter<Vec<u8>>,
    update: &invisible_keyboard::device_transport::DeviceTransportUpdate,
) {
    for candidate in &update.candidates {
        writer
            .append_transport_candidate(
                ControllerStream::from(candidate.source),
                &candidate.candidate,
                candidate.receipt_time_ns,
            )
            .unwrap();
    }
}

// This is the CLI's receipt-merger entry point, not a direct runtime push.
fn dispatch_live_ingress<A: OutputAdapter>(
    live: &mut LiveCalibratedRuntime<A>,
    transport: &mut invisible_keyboard::device_transport::DeviceTransport,
    writer: &mut SessionWriter<Vec<u8>>,
    event: &invisible_keyboard::device_transport::DeviceTransportEvent,
) -> Result<(), LiveCalibratedError<A::Error>> {
    use invisible_keyboard::device_transport::DeviceTransportEvent;
    let consumer_time_ns = match event {
        DeviceTransportEvent::Tick { host_time_ns } => *host_time_ns,
        DeviceTransportEvent::Bytes {
            receipt_time_ns, ..
        }
        | DeviceTransportEvent::Disconnected {
            receipt_time_ns, ..
        } => *receipt_time_ns,
    };
    dispatch_live_ingress_at(live, transport, writer, event, consumer_time_ns)
}

fn dispatch_live_ingress_at<A: OutputAdapter>(
    live: &mut LiveCalibratedRuntime<A>,
    transport: &mut invisible_keyboard::device_transport::DeviceTransport,
    writer: &mut SessionWriter<Vec<u8>>,
    event: &invisible_keyboard::device_transport::DeviceTransportEvent,
    consumer_time_ns: u64,
) -> Result<(), LiveCalibratedError<A::Error>> {
    let update = live.admit_transport_event(transport, event, || {}).unwrap();
    append_live_ingress_evidence(writer, update.evidence());
    live.release_transport_update(update, consumer_time_ns)
}

#[test]
fn live_cli_ingress_revokes_merge_held_faults_before_signal_and_retains_incident() {
    use invisible_keyboard::device_transport::{
        DeviceTransport, DeviceTransportConfig, DeviceTransportEvent,
    };
    use invisible_keyboard::transport::encode_transport_frame;

    let fixture = fixture();
    for hand in [Hand::Left, Hand::Right] {
        for (continuity_fault, stale_sensor) in [(false, false), (true, false), (false, true)] {
            let mut live = LiveCalibratedRuntime::new(
                &fixture.calibration_manifest,
                &fixture.runtime_manifest,
                &fixture.artifact,
                confirm_wearables_unmoved(&fixture),
                RecordingOutput::new(),
            )
            .unwrap();
            let mut transport = DeviceTransport::new(
                fixture
                    .runtime_manifest
                    .hands
                    .iter()
                    .map(|hand| hand.frame_identity().unwrap()),
                DeviceTransportConfig::default(),
            )
            .unwrap();
            let mut writer = SessionWriter::new(Vec::new(), *fixture.session.header()).unwrap();
            let mut reader = fixture.session.reader();
            let mut expected = [Vec::new(), Vec::new()];
            let mut last_receipt = 0;
            while live.report().armed_transitions == 0
                || expected.iter().any(|frames| frames.len() < 64)
            {
                let record = reader.next_record().unwrap().unwrap();
                let frame = decode_frame(&record.payload).unwrap();
                expected[frame.hand as usize].push((record.host_time_ns, record.payload));
                last_receipt = record.host_time_ns;
                dispatch_live_ingress(
                    &mut live,
                    &mut transport,
                    &mut writer,
                    &DeviceTransportEvent::Bytes {
                        source: frame.hand,
                        receipt_time_ns: record.host_time_ns,
                        bytes: encode_transport_frame(&frame).unwrap(),
                    },
                )
                .unwrap();
            }
            assert_eq!(live.report().armed_transitions, 1);
            assert_eq!(live.report().key_decisions, 0);
            let previous = &expected[hand as usize].last().unwrap().1;
            let mut fault = decode_frame(previous).unwrap();
            advance_test_frame(&mut fault);
            if continuity_fault {
                fault.sequence = fault.sequence.wrapping_add(1);
            } else if stale_sensor {
                assert_eq!(fault.sensors[0].sample_time_offset_us, 0);
                fault.sensors[0].sample_time_offset_us = -5_000;
            } else {
                fault.flags |= FLAG_ACQUISITION_OVERRUN;
            }
            let receipt = last_receipt + 5_000_000;
            let fault_payload = encode_frame(&fault).unwrap().to_vec();
            expected[hand as usize].push((receipt, fault_payload.clone()));
            let mut suffix_frame = fault;
            advance_test_frame(&mut suffix_frame);
            suffix_frame.flags = 0;
            let suffix_payload = encode_frame(&suffix_frame).unwrap().to_vec();
            expected[hand as usize].push((receipt, suffix_payload));
            let partial = vec![2, 3, 4];
            let mut chunk = encode_transport_frame(&fault).unwrap();
            chunk.extend(encode_transport_frame(&suffix_frame).unwrap());
            chunk.extend_from_slice(&partial);
            let before = live.report().clone();
            let reason = if stale_sensor {
                LiveTerminalReason::RuntimeFault(CalibratedRuntimeFault::Pipeline(
                    invisible_keyboard::pipeline::PipelineError::InvalidWindow(
                        invisible_keyboard::decoder::WindowError::SensorTimeRegression {
                            sensor: 0,
                        },
                    ),
                ))
            } else {
                LiveTerminalReason::EvidenceInvalidatingAbstention(if continuity_fault {
                    AbstainReason::SequenceDiscontinuity
                } else {
                    AbstainReason::AcquisitionFault
                })
            };
            assert!(
                matches!(
                    dispatch_live_ingress(&mut live, &mut transport, &mut writer, &DeviceTransportEvent::Bytes {
                        source: hand, receipt_time_ns: receipt, bytes: chunk,
                    }),
                    Err(LiveCalibratedError::TerminalFault(actual)) if actual == reason
                ),
                "a completed faulty frame must revoke inside the upstream receipt interval"
            );
            assert!(
                transport.pending_candidates() > 0,
                "the fault is still merge-held"
            );
            assert_eq!(live.report().decisions, before.decisions);
            assert_eq!(live.report().key_decisions, before.key_decisions);
            assert_eq!(live.report().output_events, before.output_events);
            assert_eq!(live.report().terminal_reason, Some(reason.clone()));
            assert!(live.report().output_neutrality_confirmed);
            assert!(matches!(live.shutdown(ShutdownSignal::Interrupt),
                Err(LiveCalibratedError::WearSessionRevoked(actual)) if actual == reason));
            assert!(matches!(live.tick(receipt + 50_000_000),
                Err(LiveCalibratedError::WearSessionRevoked(actual)) if actual == reason));
            let pending_before_post_cutoff = transport.pending_candidates();
            assert!(matches!(
                dispatch_live_ingress(&mut live, &mut transport, &mut writer, &DeviceTransportEvent::Bytes {
                    source: hand,
                    receipt_time_ns: receipt + 1,
                    bytes: encode_transport_frame(&suffix_frame).unwrap(),
                }),
                Err(LiveCalibratedError::WearSessionRevoked(actual)) if actual == reason
            ));
            assert_eq!(transport.pending_candidates(), pending_before_post_cutoff);
            let other_hand = if hand == Hand::Left {
                Hand::Right
            } else {
                Hand::Left
            };
            let mut backlog =
                decode_frame(&expected[other_hand as usize].last().unwrap().1).unwrap();
            advance_test_frame(&mut backlog);
            let backlog_receipt = receipt + 1;
            expected[other_hand as usize]
                .push((backlog_receipt, encode_frame(&backlog).unwrap().to_vec()));
            let update = transport
                .retain_evidence(DeviceTransportEvent::Bytes {
                    source: other_hand,
                    receipt_time_ns: backlog_receipt,
                    bytes: encode_transport_frame(&backlog).unwrap(),
                })
                .unwrap();
            append_live_ingress_evidence(&mut writer, &update);
            let update = transport
                .retain_evidence(DeviceTransportEvent::Disconnected {
                    source: hand,
                    receipt_time_ns: receipt + 2,
                })
                .unwrap();
            append_live_ingress_evidence(&mut writer, &update);
            let retained = writer.finish_finalized().unwrap();
            let mut actual = [Vec::new(), Vec::new()];
            let mut malformed = Vec::new();
            let mut retained_reader = retained.reader();
            while let Some(record) = retained_reader.next_record().unwrap() {
                if record.kind == SessionRecordKind::Decoded {
                    actual[record.source as usize].push((record.host_time_ns, record.payload));
                } else {
                    malformed.push((record.kind, record.host_time_ns, record.payload));
                }
            }
            assert_eq!(
                actual, expected,
                "whole chunk and admitted backlog retain once, with original receipts"
            );
            assert_eq!(
                malformed,
                vec![(SessionRecordKind::Unterminated, receipt, partial)]
            );
            assert_eq!(
                actual[hand as usize]
                    .iter()
                    .filter(|(_, payload)| *payload == fault_payload)
                    .count(),
                1
            );
            let stopped = live.finish().unwrap();
            assert_eq!(stopped.report.terminal_reason, Some(reason));
            assert!(!stopped.report.finished_cleanly);
            assert!(
                stopped
                    .output
                    .events()
                    .iter()
                    .all(|event| event.state != EventState::Down)
            );
        }
    }
}

#[test]
fn live_cli_ingress_healthy_receipt_merger_preserves_expected_output() {
    use invisible_keyboard::device_transport::{
        DeviceTransport, DeviceTransportConfig, DeviceTransportEvent,
    };
    use invisible_keyboard::transport::encode_transport_frame;

    let fixture = fixture();
    let mut live = LiveCalibratedRuntime::new(
        &fixture.calibration_manifest,
        &fixture.runtime_manifest,
        &fixture.artifact,
        confirm_wearables_unmoved(&fixture),
        RecordingOutput::new(),
    )
    .unwrap();
    let mut transport = DeviceTransport::new(
        fixture
            .runtime_manifest
            .hands
            .iter()
            .map(|hand| hand.frame_identity().unwrap()),
        DeviceTransportConfig::default(),
    )
    .unwrap();
    let mut writer = SessionWriter::new(Vec::new(), *fixture.session.header()).unwrap();
    let mut reader = fixture.session.reader();
    let mut last_receipt = 0;
    while let Some(record) = reader.next_record().unwrap() {
        let frame = decode_frame(&record.payload).unwrap();
        last_receipt = record.host_time_ns;
        dispatch_live_ingress(
            &mut live,
            &mut transport,
            &mut writer,
            &DeviceTransportEvent::Bytes {
                source: frame.hand,
                receipt_time_ns: record.host_time_ns,
                bytes: encode_transport_frame(&frame).unwrap(),
            },
        )
        .unwrap();
        // Match the CLI's busy-input path while the receipt merger still holds
        // newer candidates outside the runtime's mapped-time queue.
        live.tick(last_receipt).unwrap();
    }
    let release_time = last_receipt + 50_000_000;
    dispatch_live_ingress(
        &mut live,
        &mut transport,
        &mut writer,
        &DeviceTransportEvent::Tick {
            host_time_ns: release_time,
        },
    )
    .unwrap();
    live.tick(release_time).unwrap();
    let stopped = live.finish().unwrap();
    assert_eq!(stopped.report.armed_transitions, 1);
    assert_eq!(stopped.report.key_decisions, 2);
    assert_eq!(
        stopped
            .output
            .events()
            .iter()
            .filter(|event| event.state == EventState::Down)
            .map(|event| event.usage)
            .collect::<Vec<_>>(),
        [KeyUsage::A, KeyUsage::H]
    );
    assert_eq!(
        writer.finish_finalized().unwrap().summary(),
        fixture.session.summary()
    );
}

fn jittered_live_ingress_fixture() -> (
    LiveCalibratedRuntime<RecordingOutput>,
    invisible_keyboard::device_transport::DeviceTransport,
    SessionWriter<Vec<u8>>,
    u64,
) {
    jittered_live_ingress_fixture_at(0, 0)
}

fn jittered_live_ingress_fixture_at(
    consumer_lag_ns: u64,
    trailing_frames: u32,
) -> (
    LiveCalibratedRuntime<RecordingOutput>,
    invisible_keyboard::device_transport::DeviceTransport,
    SessionWriter<Vec<u8>>,
    u64,
) {
    use invisible_keyboard::device_transport::{
        DeviceTransport, DeviceTransportConfig, DeviceTransportEvent,
    };
    use invisible_keyboard::transport::encode_transport_frame;

    let fixture = fixture();
    let mut live = LiveCalibratedRuntime::new(
        &fixture.calibration_manifest,
        &fixture.runtime_manifest,
        &fixture.artifact,
        confirm_wearables_unmoved(&fixture),
        RecordingOutput::new(),
    )
    .unwrap();
    let identities = std::array::from_fn::<_, 2, _>(|index| {
        fixture.runtime_manifest.hands[index]
            .frame_identity()
            .unwrap()
    });
    let mut transport = DeviceTransport::new(identities, DeviceTransportConfig::default()).unwrap();
    let mut writer = SessionWriter::new(Vec::new(), *fixture.session.header()).unwrap();
    let configs = [
        SyntheticConfig::new(Hand::Left, 0x0052_504c_594c).with_noise(0),
        SyntheticConfig::new(Hand::Right, 0x0052_504c_5952).with_noise(0),
    ];
    let positions = ["A", "H"].map(|label| {
        all_slots()
            .iter()
            .find(|slot| slot.label == label)
            .unwrap()
            .position
    });
    let idle =
        std::array::from_fn::<_, 2, _>(|index| configs[index].idle_window(101).frames()[0].sensors);
    let impulses = std::array::from_fn::<_, 2, _>(|index| {
        configs[index].tap_window(positions[index], 100).frames()[15].sensors
    });
    let first_device_time_us = 1_000_000;
    let commit_index = 83_u32;
    let commit_receipt_ns =
        5_000_000_000 + (first_device_time_us + u64::from(commit_index) * 5_000) * 1_000;
    for index in 0..=commit_index + trailing_frames {
        let left_receipt_ns =
            5_000_000_000 + (first_device_time_us + u64::from(index) * 5_000) * 1_000;
        // Both streams are clean 200 Hz. Only the right commit receipt gets
        // 400 us jitter: its fitted time stays earlier than the left commit,
        // but the receipt merger releases the left commit first.
        let right_receipt_ns =
            left_receipt_ns - 100_000 + if index == commit_index { 400_000 } else { 0 };
        let order = if index == commit_index {
            [0, 1]
        } else {
            [1, 0]
        };
        for hand_index in order {
            let identity = identities[hand_index];
            let frame = SensorFrame {
                hand: identity.hand,
                controller_id_sha256: identity.controller_id_sha256,
                boot_id: identity.boot_id,
                flags: 0,
                valid_sensor_mask: ALL_SENSORS_VALID,
                sequence: index,
                device_time_us: first_device_time_us + u64::from(index) * 5_000,
                sensors: if index == 80 {
                    impulses[hand_index]
                } else {
                    idle[hand_index]
                },
            };
            let receipt_time_ns = if hand_index == 0 {
                left_receipt_ns
            } else {
                right_receipt_ns
            };
            dispatch_live_ingress_at(
                &mut live,
                &mut transport,
                &mut writer,
                &DeviceTransportEvent::Bytes {
                    source: frame.hand,
                    receipt_time_ns,
                    bytes: encode_transport_frame(&frame).unwrap(),
                },
                receipt_time_ns + consumer_lag_ns,
            )
            .unwrap();
            live.tick(receipt_time_ns + consumer_lag_ns).unwrap();
        }
    }
    assert_eq!(live.report().armed_transitions, 1);
    assert_eq!(live.report().key_decisions, 0);
    (live, transport, writer, commit_receipt_ns)
}

#[test]
fn live_cli_ingress_jitter_inversion_preserves_exact_core28_output() {
    use invisible_keyboard::device_transport::DeviceTransportEvent;

    let (mut live, mut transport, mut writer, commit_receipt_ns) = jittered_live_ingress_fixture();
    // Follow the actual receive loop: admit, record, release, then consumer
    // tick for every timeout or byte input. The first timeout releases only
    // the left commit; empty delimiters then advance the upstream watermark.
    for event in [
        DeviceTransportEvent::Tick {
            host_time_ns: commit_receipt_ns + 50_000_000,
        },
        DeviceTransportEvent::Bytes {
            source: Hand::Left,
            receipt_time_ns: commit_receipt_ns + 50_150_000,
            bytes: vec![0],
        },
        DeviceTransportEvent::Bytes {
            source: Hand::Right,
            receipt_time_ns: commit_receipt_ns + 50_300_000,
            bytes: vec![0],
        },
        DeviceTransportEvent::Tick {
            host_time_ns: commit_receipt_ns + 70_300_000,
        },
    ] {
        let consumer_time_ns = match &event {
            DeviceTransportEvent::Tick { host_time_ns } => *host_time_ns,
            DeviceTransportEvent::Bytes {
                receipt_time_ns, ..
            } => *receipt_time_ns,
            _ => unreachable!(),
        };
        dispatch_live_ingress(&mut live, &mut transport, &mut writer, &event).unwrap();
        live.tick(consumer_time_ns).unwrap();
    }
    let execution = live.finish().unwrap();
    assert_eq!(execution.report.terminal_reason, None);
    assert!(execution.report.runtime_faults.is_empty());
    assert_eq!(execution.report.key_decisions, 2);
    // The 51-point, 250 ms fit moves the jittered right commit to T-69.532 us.
    // Canonical output truncates to microseconds and preserves both full taps.
    let left_time_us = commit_receipt_ns / 1_000;
    assert_eq!(
        execution.output.events(),
        [
            KeyEvent::down(KeyUsage::H, left_time_us - 70),
            KeyEvent::up(KeyUsage::H, left_time_us - 70),
            KeyEvent::down(KeyUsage::A, left_time_us),
            KeyEvent::up(KeyUsage::A, left_time_us),
        ]
    );
    assert_eq!(
        writer
            .finish_finalized()
            .unwrap()
            .summary()
            .decoded_candidates,
        168
    );
    assert!(execution.report.finished_cleanly);
    assert!(execution.report.final_key_state_neutral);
    assert!(execution.report.output_neutrality_confirmed);
}

#[test]
fn live_cli_ingress_busy_watchdog_revokes_before_safe_horizon_flush() {
    use invisible_keyboard::device_transport::DeviceTransportEvent;

    let (mut live, mut transport, mut writer, commit_receipt_ns) = jittered_live_ingress_fixture();
    // Candidate-free traffic can release held commits and advance the safe
    // horizon, but may not renew valid-frame health. A stalled consumer must
    // neutralize before that newly safe backlog can produce either tap.
    let consumer_time_ns = commit_receipt_ns + LIVE_WATCHDOG_NS + 400_000;
    let release = dispatch_live_ingress(
        &mut live,
        &mut transport,
        &mut writer,
        &DeviceTransportEvent::Bytes {
            source: Hand::Left,
            receipt_time_ns: consumer_time_ns,
            bytes: vec![0],
        },
    );
    assert_eq!(live.report().output_events, 0);
    let reason = LiveTerminalReason::WatchdogTimeout {
        left_expired: true,
        right_expired: true,
    };
    assert!(matches!(
        release,
        Err(LiveCalibratedError::TerminalFault(actual)) if actual == reason
    ));
    assert_eq!(live.report().watchdog_timeouts, 1);
    assert_eq!(live.report().terminal_reason, Some(reason.clone()));
    assert_eq!(live.report().output_events, 0);
    assert!(matches!(
        live.tick(consumer_time_ns + 1),
        Err(LiveCalibratedError::WearSessionRevoked(actual)) if actual == reason
    ));
    let execution = live.finish().unwrap();
    assert!(execution.output.events().is_empty());
    assert!(execution.report.final_key_state_neutral);
    assert!(execution.report.output_neutrality_confirmed);
    assert_eq!(
        writer
            .finish_finalized()
            .unwrap()
            .summary()
            .decoded_candidates,
        168
    );
}

#[test]
fn live_post_evidence_consumer_delay_revokes_before_candidate_flush() {
    use invisible_keyboard::device_transport::DeviceTransportEvent;
    use invisible_keyboard::transport::encode_transport_frame;

    let (mut live, mut transport, mut writer, commit_receipt_ns) =
        jittered_live_ingress_fixture_at(0, 4);
    assert_eq!(live.report().output_events, 0);
    let event = DeviceTransportEvent::Bytes {
        source: Hand::Left,
        receipt_time_ns: commit_receipt_ns + 75_000_000,
        bytes: vec![0],
    };
    let update = live
        .admit_transport_event(&mut transport, &event, || {})
        .unwrap();
    assert!(update.evidence().faults.is_empty());
    assert_eq!(
        update
            .evidence()
            .candidates
            .iter()
            .map(|candidate| candidate.receipt_time_ns)
            .max(),
        Some(commit_receipt_ns + 20_000_000)
    );
    let last_left = update
        .evidence()
        .candidates
        .iter()
        .rfind(|candidate| candidate.source == Hand::Left)
        .unwrap();
    let TransportCandidate::Decoded(payload) = &last_left.candidate else {
        panic!("clean trailing frame must decode");
    };
    let mut future_frame = decode_frame(payload).unwrap();
    advance_test_frame(&mut future_frame);
    append_live_ingress_evidence(&mut writer, update.evidence());
    // Consumer resumes after evidence append, not at the event's raw receipt.
    let consumer_time_ns = commit_receipt_ns + 200_000_000;
    let release = live.release_transport_update(update, consumer_time_ns);
    assert_eq!(live.report().output_events, 0);
    let reason = LiveTerminalReason::WatchdogTimeout {
        left_expired: true,
        right_expired: true,
    };
    assert!(matches!(
        release,
        Err(LiveCalibratedError::TerminalFault(actual)) if actual == reason
    ));
    assert_eq!(live.report().watchdog_timeouts, 1);
    assert_eq!(live.report().terminal_reason, Some(reason.clone()));
    assert!(matches!(
        live.tick(consumer_time_ns),
        Err(LiveCalibratedError::WearSessionRevoked(actual)) if actual == reason
    ));
    let held_before_future = transport.pending_candidates();
    let future = live
        .admit_transport_event(
            &mut transport,
            &DeviceTransportEvent::Bytes {
                source: Hand::Left,
                receipt_time_ns: consumer_time_ns + 5_000_000,
                bytes: encode_transport_frame(&future_frame).unwrap(),
            },
            || {},
        )
        .unwrap();
    assert!(future.evidence().candidates.is_empty());
    assert_eq!(transport.pending_candidates(), held_before_future);
    assert!(matches!(
        live.release_transport_update(future, consumer_time_ns + 5_000_000),
        Err(LiveCalibratedError::WearSessionRevoked(actual)) if actual == reason
    ));
    let execution = live.finish().unwrap();
    assert!(execution.output.events().is_empty());
    assert!(execution.report.final_key_state_neutral);
    assert!(execution.report.output_neutrality_confirmed);
    let session = writer.finish_finalized().unwrap();
    assert_eq!(session.summary().decoded_candidates, 176);
    let mut reader = session.reader();
    let mut trailing = Vec::new();
    while let Some(record) = reader.next_record().unwrap() {
        let frame = decode_frame(&record.payload).unwrap();
        if frame.sequence >= 84 {
            trailing.push((frame.hand, frame.sequence, record.host_time_ns));
        }
    }
    assert_eq!(
        trailing,
        (84_u32..=87)
            .flat_map(|index| {
                [Hand::Right, Hand::Left].map(|hand| {
                    (
                        hand,
                        index,
                        commit_receipt_ns + u64::from(index - 83) * 5_000_000
                            - if hand == Hand::Right { 100_000 } else { 0 },
                    )
                })
            })
            .collect::<Vec<_>>()
    );
}

#[test]
fn live_post_evidence_healthy_delays_preserve_exact_core28_output() {
    use invisible_keyboard::device_transport::DeviceTransportEvent;

    for consumer_lag_ns in [0, 49_000_000] {
        let (mut live, mut transport, mut writer, commit_receipt_ns) =
            jittered_live_ingress_fixture_at(consumer_lag_ns, 4);
        // Almost 50 ms publication/merger residence plus almost 50 ms
        // consumer lag must not expire health measured on old released frames.
        let receipt_time_ns = commit_receipt_ns + 70_000_000;
        let consumer_time_ns = receipt_time_ns + consumer_lag_ns;
        dispatch_live_ingress_at(
            &mut live,
            &mut transport,
            &mut writer,
            &DeviceTransportEvent::Bytes {
                source: Hand::Left,
                receipt_time_ns,
                bytes: vec![0],
            },
            consumer_time_ns,
        )
        .unwrap();
        live.tick(consumer_time_ns).unwrap();
        let execution = live.finish().unwrap();
        assert_eq!(execution.report.terminal_reason, None);
        assert!(execution.report.runtime_faults.is_empty());
        assert_eq!(execution.report.armed_transitions, 1);
        assert_eq!(execution.report.key_decisions, 2);
        let left_time_us = commit_receipt_ns / 1_000;
        assert_eq!(
            execution.output.events(),
            [
                KeyEvent::down(KeyUsage::H, left_time_us - 70),
                KeyEvent::up(KeyUsage::H, left_time_us - 70),
                KeyEvent::down(KeyUsage::A, left_time_us),
                KeyEvent::up(KeyUsage::A, left_time_us),
            ]
        );
        assert_eq!(
            writer
                .finish_finalized()
                .unwrap()
                .summary()
                .decoded_candidates,
            176
        );
        assert!(execution.report.finished_cleanly);
        assert!(execution.report.final_key_state_neutral);
        assert!(execution.report.output_neutrality_confirmed);
    }
}

#[test]
fn live_stale_bootstrap_warmup_cannot_arm_after_evidence_delay() {
    use invisible_keyboard::device_transport::{
        DeviceTransport, DeviceTransportConfig, DeviceTransportEvent,
    };
    use invisible_keyboard::transport::encode_transport_frame;

    let fixture = fixture();
    let mut live = LiveCalibratedRuntime::new(
        &fixture.calibration_manifest,
        &fixture.runtime_manifest,
        &fixture.artifact,
        confirm_wearables_unmoved(&fixture),
        RecordingOutput::new(),
    )
    .unwrap();
    let mut transport = DeviceTransport::new(
        fixture
            .runtime_manifest
            .hands
            .iter()
            .map(|hand| hand.frame_identity().unwrap()),
        DeviceTransportConfig {
            merge_lateness_ns: 0,
            ..DeviceTransportConfig::default()
        },
    )
    .unwrap();
    let mut writer = SessionWriter::new(Vec::new(), *fixture.session.header()).unwrap();
    let mut reader = fixture.session.reader();
    let mut updates = Vec::new();
    let mut last_receipt = 0;
    for _ in 0..104 {
        let record = reader.next_record().unwrap().unwrap();
        let frame = decode_frame(&record.payload).unwrap();
        last_receipt = record.host_time_ns;
        let update = live
            .admit_transport_event(
                &mut transport,
                &DeviceTransportEvent::Bytes {
                    source: frame.hand,
                    receipt_time_ns: record.host_time_ns,
                    bytes: encode_transport_frame(&frame).unwrap(),
                },
                || {},
            )
            .unwrap();
        append_live_ingress_evidence(&mut writer, update.evidence());
        updates.push(update);
    }
    assert_eq!(live.report().armed_transitions, 0);
    let consumer_time_ns = last_receipt + 200_000_000;
    for update in updates {
        live.release_transport_update(update, consumer_time_ns)
            .expect("stale warmup remains nonauthorizing, not a terminal fault");
        assert_eq!(live.report().armed_transitions, 0);
        assert_eq!(live.report().output_events, 0);
    }
    live.tick(consumer_time_ns)
        .expect("stale nonauthorizing warmup cannot flush output");
    let execution = live.finish().unwrap();
    assert_eq!(execution.report.terminal_reason, None);
    assert_eq!(execution.report.watchdog_timeouts, 0);
    assert_eq!(execution.report.decoded_frames, 104);
    assert_eq!(execution.report.clock_warmup_frames, 104);
    assert!(execution.report.finished_cleanly);
    assert!(execution.left_clock_fit.is_some());
    assert!(execution.right_clock_fit.is_some());
    assert!(execution.output.events().is_empty());
    assert!(execution.report.final_key_state_neutral);
    assert!(execution.report.output_neutrality_confirmed);
    assert_eq!(
        writer
            .finish_finalized()
            .unwrap()
            .summary()
            .decoded_candidates,
        104
    );
}

fn push_live_candidate<A: OutputAdapter>(
    live: &mut LiveCalibratedRuntime<A>,
    candidate: TransportCandidate,
    receipt_time_ns: u64,
) -> Result<(), LiveCalibratedError<A::Error>> {
    push_live_candidate_at(live, candidate, receipt_time_ns, receipt_time_ns)
}

fn push_live_candidate_at<A: OutputAdapter>(
    live: &mut LiveCalibratedRuntime<A>,
    candidate: TransportCandidate,
    receipt_time_ns: u64,
    consumer_time_ns: u64,
) -> Result<(), LiveCalibratedError<A::Error>> {
    use invisible_keyboard::device_transport::{
        DeviceTransport, DeviceTransportConfig, DeviceTransportEvent,
    };

    static INPUT: std::sync::LazyLock<(
        [invisible_keyboard::protocol::FrameIdentity; 2],
        SessionHeader,
    )> = std::sync::LazyLock::new(|| {
        let fixture = fixture();
        (
            std::array::from_fn(|index| {
                fixture.runtime_manifest.hands[index]
                    .frame_identity()
                    .unwrap()
            }),
            *fixture.session.header(),
        )
    });
    let (identities, header) = &*INPUT;
    let (source, bytes, unterminated) = match candidate {
        TransportCandidate::Decoded(payload) => {
            let frame = decode_frame(&payload).unwrap();
            (
                frame.hand,
                invisible_keyboard::transport::encode_transport_frame(&frame).unwrap(),
                false,
            )
        }
        TransportCandidate::Malformed { mut encoded } => {
            encoded.push(0);
            (Hand::Left, encoded, false)
        }
        TransportCandidate::Unterminated { encoded } => (Hand::Left, encoded, true),
        TransportCandidate::Oversized {
            mut prefix,
            actual_len,
        } => {
            prefix.resize(actual_len, 1);
            prefix.push(0);
            (Hand::Left, prefix, false)
        }
    };
    // Existing runtime-focused tests use immediate receipt release. The
    // ingress regressions above retain the CLI's real 50 ms transport policy.
    let mut transport = DeviceTransport::new(
        *identities,
        DeviceTransportConfig {
            merge_lateness_ns: 0,
            ..DeviceTransportConfig::default()
        },
    )
    .unwrap();
    let mut writer = SessionWriter::new(Vec::new(), *header).unwrap();
    dispatch_live_ingress_at(
        live,
        &mut transport,
        &mut writer,
        &DeviceTransportEvent::Bytes {
            source,
            receipt_time_ns,
            bytes,
        },
        consumer_time_ns,
    )?;
    if unterminated {
        dispatch_live_ingress_at(
            live,
            &mut transport,
            &mut writer,
            &DeviceTransportEvent::Disconnected {
                source,
                receipt_time_ns: receipt_time_ns.checked_add(1).unwrap(),
            },
            consumer_time_ns.max(receipt_time_ns.checked_add(1).unwrap()),
        )?;
    }
    writer.finish_finalized().unwrap();
    Ok(())
}

#[test]
fn live_ingress_update_cannot_be_released_by_another_authority() {
    use invisible_keyboard::device_transport::{
        DeviceTransport, DeviceTransportConfig, DeviceTransportEvent,
    };
    use invisible_keyboard::transport::encode_transport_frame;

    let fixture = fixture();
    let mut owner = LiveCalibratedRuntime::new(
        &fixture.calibration_manifest,
        &fixture.runtime_manifest,
        &fixture.artifact,
        confirm_wearables_unmoved(&fixture),
        RecordingOutput::new(),
    )
    .unwrap();
    let mut other = LiveCalibratedRuntime::new(
        &fixture.calibration_manifest,
        &fixture.runtime_manifest,
        &fixture.artifact,
        confirm_wearables_unmoved(&fixture),
        RecordingOutput::new(),
    )
    .unwrap();
    let mut transport = DeviceTransport::new(
        fixture
            .runtime_manifest
            .hands
            .iter()
            .map(|hand| hand.frame_identity().unwrap()),
        DeviceTransportConfig {
            merge_lateness_ns: 0,
            ..DeviceTransportConfig::default()
        },
    )
    .unwrap();
    let record = fixture.session.reader().next_record().unwrap().unwrap();
    let frame = decode_frame(&record.payload).unwrap();
    let update = owner
        .admit_transport_event(
            &mut transport,
            &DeviceTransportEvent::Bytes {
                source: frame.hand,
                receipt_time_ns: record.host_time_ns,
                bytes: encode_transport_frame(&frame).unwrap(),
            },
            || {},
        )
        .unwrap();
    assert_eq!(update.evidence().candidates.len(), 1);
    let mut evidence = SessionWriter::new(Vec::new(), *fixture.session.header()).unwrap();
    append_live_ingress_evidence(&mut evidence, update.evidence());
    assert!(matches!(
        other.release_transport_update(update, record.host_time_ns),
        Err(LiveCalibratedError::ForeignAdmission)
    ));
    assert_eq!(other.report().total_candidates, 0);
    assert_eq!(other.report().terminal_reason, None);
    assert!(other.finish().unwrap().output.events().is_empty());
    assert!(owner.finish().unwrap().output.events().is_empty());
}

#[test]
fn live_ingress_closes_admission_after_neutralization_before_releasing_any_evidence() {
    use invisible_keyboard::device_transport::{
        DeviceTransport, DeviceTransportConfig, DeviceTransportEvent,
    };
    use invisible_keyboard::transport::encode_transport_frame;

    let fixture = fixture();
    let observed = SharedRecordingOutput::default();
    let mut live = LiveCalibratedRuntime::new(
        &fixture.calibration_manifest,
        &fixture.runtime_manifest,
        &fixture.artifact,
        confirm_wearables_unmoved(&fixture),
        observed.clone(),
    )
    .unwrap();
    let mut transport = DeviceTransport::new(
        fixture
            .runtime_manifest
            .hands
            .iter()
            .map(|hand| hand.frame_identity().unwrap()),
        DeviceTransportConfig::default(),
    )
    .unwrap();
    let record = fixture.session.reader().next_record().unwrap().unwrap();
    let mut frame = decode_frame(&record.payload).unwrap();
    frame.flags |= FLAG_ACQUISITION_OVERRUN;
    let mut closed = false;
    let update = live
        .admit_transport_event(
            &mut transport,
            &DeviceTransportEvent::Bytes {
                source: frame.hand,
                receipt_time_ns: record.host_time_ns,
                bytes: encode_transport_frame(&frame).unwrap(),
            },
            || {
                assert_eq!(
                    observed.release_all_calls(),
                    2,
                    "neutralize before the input cutoff callback"
                );
                assert_eq!(observed.event_count(), 0);
                closed = true;
            },
        )
        .unwrap();
    assert!(closed);
    assert!(
        update.evidence().candidates.is_empty(),
        "the offending frame is still upstream-held"
    );
    assert_eq!(transport.pending_candidates(), 1);
    assert_eq!(
        live.report().terminal_reason,
        Some(LiveTerminalReason::EvidenceInvalidatingAbstention(
            AbstainReason::AcquisitionFault
        ))
    );
    assert!(matches!(
        live.release_transport_update(update, record.host_time_ns),
        Err(LiveCalibratedError::TerminalFault(_))
    ));
    assert_eq!(live.report().total_candidates, 0);
}

fn assert_live_malformed_ingress_revokes(later_acquisition_fault: bool) {
    use invisible_keyboard::{
        device_transport::{DeviceTransport, DeviceTransportConfig, DeviceTransportEvent},
        protocol::FrameError,
        transport::encode_transport_frame,
    };

    let fixture = fixture();
    for (wire, kind, payload, fault) in [
        (
            vec![0xff, 0],
            SessionRecordKind::Malformed,
            vec![0xff],
            CalibratedRuntimeFault::Transport(TransportFault::Malformed),
        ),
        (
            vec![2, 7, 0],
            SessionRecordKind::Decoded,
            vec![7],
            CalibratedRuntimeFault::Frame(FrameError::WrongLength { actual: 1 }),
        ),
    ] {
        let mut live = LiveCalibratedRuntime::new(
            &fixture.calibration_manifest,
            &fixture.runtime_manifest,
            &fixture.artifact,
            confirm_wearables_unmoved(&fixture),
            RecordingOutput::new(),
        )
        .unwrap();
        let mut transport = DeviceTransport::new(
            fixture
                .runtime_manifest
                .hands
                .iter()
                .map(|hand| hand.frame_identity().unwrap()),
            DeviceTransportConfig::default(),
        )
        .unwrap();
        let mut writer = SessionWriter::new(Vec::new(), *fixture.session.header()).unwrap();
        let mut reader = fixture.session.reader();
        let mut expected = [Vec::new(), Vec::new()];
        let mut last_left = None;
        let mut last_receipt = 0;
        while expected.iter().any(|records| records.len() < 64) {
            let record = reader.next_record().unwrap().unwrap();
            let frame = decode_frame(&record.payload).unwrap();
            expected[frame.hand as usize].push((record.kind, record.host_time_ns, record.payload));
            if frame.hand == Hand::Left {
                last_left = Some(frame);
            }
            last_receipt = record.host_time_ns;
            dispatch_live_ingress(
                &mut live,
                &mut transport,
                &mut writer,
                &DeviceTransportEvent::Bytes {
                    source: frame.hand,
                    receipt_time_ns: record.host_time_ns,
                    bytes: encode_transport_frame(&frame).unwrap(),
                },
            )
            .unwrap();
        }
        assert_eq!(live.report().armed_transitions, 1);
        assert_eq!(live.report().key_decisions, 0);
        let before = live.report().clone();
        let receipt = last_receipt + 5_000_000;
        expected[Hand::Left as usize].push((kind, receipt, payload));
        let mut next = last_left.unwrap();
        advance_test_frame(&mut next);
        let mut chunk = wire;
        if later_acquisition_fault {
            next.flags = FLAG_ACQUISITION_OVERRUN;
            expected[Hand::Left as usize].push((
                SessionRecordKind::Decoded,
                receipt,
                encode_frame(&next).unwrap().to_vec(),
            ));
            chunk.extend(encode_transport_frame(&next).unwrap());
            advance_test_frame(&mut next);
            next.flags = 0;
        }
        expected[Hand::Left as usize].push((
            SessionRecordKind::Decoded,
            receipt,
            encode_frame(&next).unwrap().to_vec(),
        ));
        chunk.extend(encode_transport_frame(&next).unwrap());
        let partial = vec![2, 3, 4];
        expected[Hand::Left as usize].push((
            SessionRecordKind::Unterminated,
            receipt,
            partial.clone(),
        ));
        chunk.extend(partial);
        let reason = LiveTerminalReason::RuntimeFault(fault.clone());
        assert!(
            matches!(
                dispatch_live_ingress(&mut live, &mut transport, &mut writer, &DeviceTransportEvent::Bytes {
                    source: Hand::Left, receipt_time_ns: receipt, bytes: chunk,
                }),
                Err(LiveCalibratedError::TerminalFault(actual)) if actual == reason
            ),
            "malformed ingress must win before receipt release or a later acquisition fault"
        );
        assert!(transport.pending_candidates() > 0);
        assert_eq!(live.report().invalid_candidates, 1);
        assert_eq!(live.report().runtime_faults, vec![fault]);
        assert_eq!(live.report().decisions, before.decisions);
        assert_eq!(live.report().output_events, before.output_events);
        assert!(live.report().output_neutrality_confirmed);
        assert!(matches!(live.shutdown(ShutdownSignal::Interrupt),
            Err(LiveCalibratedError::WearSessionRevoked(actual)) if actual == reason));
        assert!(matches!(live.tick(receipt + 50_000_000),
            Err(LiveCalibratedError::WearSessionRevoked(actual)) if actual == reason));
        let update = transport
            .retain_evidence(DeviceTransportEvent::Disconnected {
                source: Hand::Left,
                receipt_time_ns: receipt + 1,
            })
            .unwrap();
        append_live_ingress_evidence(&mut writer, &update);
        let retained = writer.finish_finalized().unwrap();
        let mut actual = [Vec::new(), Vec::new()];
        let mut reader = retained.reader();
        while let Some(record) = reader.next_record().unwrap() {
            actual[record.source as usize].push((record.kind, record.host_time_ns, record.payload));
        }
        assert_eq!(
            actual, expected,
            "malformed evidence and the whole admitted chunk retain exactly once"
        );
        let stopped = live.finish().unwrap();
        assert_eq!(stopped.report.terminal_reason, Some(reason));
        assert!(!stopped.report.finished_cleanly);
        assert!(stopped.output.events().is_empty());
    }
}

#[test]
fn live_merge_held_malformed_candidates_revoke_before_queued_output() {
    assert_live_malformed_ingress_revokes(false);
}

#[test]
fn live_malformed_ingress_precedes_a_later_acquisition_fault() {
    assert_live_malformed_ingress_revokes(true);
}

// Raw sensor offsets stay in device microseconds under either clock slope.
fn clock_domain_fixture(slope_ns_per_us: u64, repeated_raw_sensor: bool) -> ReplayFixture {
    let mut fixture = fixture();
    let configs = [
        SyntheticConfig::new(Hand::Left, 0x0052_504c_594c).with_noise(0),
        SyntheticConfig::new(Hand::Right, 0x0052_504c_5952).with_noise(0),
    ];
    let positions = ["A", "H"].map(|label| {
        all_slots()
            .iter()
            .find(|slot| slot.label == label)
            .unwrap()
            .position
    });
    let idle =
        std::array::from_fn::<_, 2, _>(|index| configs[index].idle_window(101).frames()[0].sensors);
    let impulses = std::array::from_fn::<_, 2, _>(|index| {
        configs[index].tap_window(positions[index], 100).frames()[15].sensors
    });
    let mut writer = SessionWriter::new(Vec::new(), *fixture.session.header()).unwrap();
    for index in 0..=100_u32 {
        for hand_index in 0..2 {
            let identity = fixture.runtime_manifest.hands[hand_index]
                .frame_identity()
                .unwrap();
            let mut frame = SensorFrame {
                hand: identity.hand,
                controller_id_sha256: identity.controller_id_sha256,
                boot_id: identity.boot_id,
                flags: 0,
                valid_sensor_mask: ALL_SENSORS_VALID,
                sequence: index,
                device_time_us: if repeated_raw_sensor {
                    100_000 + u64::from(index) * 5_000
                } else {
                    1_000_000 + u64::from(index) * 5_000 + u64::from(index >= 61)
                },
                sensors: if index == 80 {
                    impulses[hand_index]
                } else {
                    idle[hand_index]
                },
            };
            if (repeated_raw_sensor && index == 1) || (!repeated_raw_sensor && index == 61) {
                frame.sensors[0].sample_time_offset_us = -5_000;
            }
            let receipt_ns = 5_000_000_000
                + u64::try_from(hand_index).unwrap() * 1_000_000
                + frame.device_time_us * slope_ns_per_us;
            writer
                .append_frame(ControllerStream::from(frame.hand), &frame, receipt_ns)
                .unwrap();
        }
    }
    fixture.session = writer.finish_finalized().unwrap();
    fixture.clocks =
        ClockEpochSet::fit_session(&fixture.runtime_manifest, &fixture.session).unwrap();
    for hand in [Hand::Left, Hand::Right] {
        assert!(
            (fixture
                .clocks
                .fit_for(hand)
                .unwrap()
                .report()
                .slope_ns_per_us
                - slope_ns_per_us as f64)
                .abs()
                < 0.000_001
        );
    }
    fixture
}

#[test]
fn replay_positive_drift_rejects_repeated_raw_sensor_before_mapping() {
    let fixture = clock_domain_fixture(1_001, true);
    let observed = SharedRecordingOutput::default();
    let result = run_calibrated_replay(
        &fixture.calibration_manifest,
        &fixture.runtime_manifest,
        &fixture.clocks,
        &fixture.artifact,
        &fixture.held_out_validation,
        &fixture.session,
        observed.clone(),
    );
    let error = match result {
        Err(error) => error,
        Ok(_) => panic!("positive drift must not hide repeated raw sensor time"),
    };
    assert!(
        matches!(
            error,
            CalibratedReplayError::RawFrameStructure {
                ordinal: 2,
                hand: Hand::Left,
                error: invisible_keyboard::decoder::WindowError::SensorTimeRegression { sensor: 0 },
            }
        ),
        "{error:?}"
    );
    assert_eq!(observed.event_count(), 0);
    assert_eq!(
        observed.release_all_calls(),
        2,
        "raw admission forces neutral before returning"
    );
    let mut reader = fixture.session.reader();
    let previous = reader.next_record().unwrap().unwrap().decode().unwrap();
    reader.next_record().unwrap().unwrap();
    let repeated = reader.next_record().unwrap().unwrap().decode().unwrap();
    assert_eq!(previous.device_time_us, 100_000);
    assert_eq!(repeated.device_time_us, 105_000);
    assert_eq!(repeated.sensors[0].sample_time_offset_us, -5_000);
}

fn assert_clock_domain_events(events: &[KeyEvent], slope_ns_per_us: u64) {
    let commit_device_time_us = 1_000_000 + 83 * 5_000 + 1;
    let left_time_us = (5_000_000_000 + commit_device_time_us * slope_ns_per_us) / 1_000;
    assert_eq!(
        events,
        [
            KeyEvent::down(KeyUsage::A, left_time_us),
            KeyEvent::up(KeyUsage::A, left_time_us),
            KeyEvent::down(KeyUsage::H, left_time_us + 1_000),
            KeyEvent::up(KeyUsage::H, left_time_us + 1_000),
        ]
    );
}

#[test]
fn replay_negative_drift_preserves_raw_sensor_progress_and_mapped_output() {
    let fixture = clock_domain_fixture(999, false);
    let execution = run_calibrated_replay(
        &fixture.calibration_manifest,
        &fixture.runtime_manifest,
        &fixture.clocks,
        &fixture.artifact,
        &fixture.held_out_validation,
        &fixture.session,
        RecordingOutput::new(),
    )
    .unwrap();
    assert!(
        execution.report.runtime_faults.is_empty(),
        "{:?}",
        execution.report.runtime_faults
    );
    assert_eq!(execution.report.key_decisions, 2);
    assert_eq!(execution.report.decoded_frames, 202);
    assert_clock_domain_events(execution.output.events(), 999);
    assert!(execution.report.final_key_state_neutral);
}

#[test]
fn live_negative_drift_consumes_allowed_skew_boundary_and_preserves_mapped_output() {
    use invisible_keyboard::device_transport::{
        DeviceTransport, DeviceTransportConfig, DeviceTransportEvent,
    };
    use invisible_keyboard::transport::encode_transport_frame;

    let fixture = clock_domain_fixture(999, false);
    let mut live = LiveCalibratedRuntime::new(
        &fixture.calibration_manifest,
        &fixture.runtime_manifest,
        &fixture.artifact,
        confirm_wearables_unmoved(&fixture),
        RecordingOutput::new(),
    )
    .unwrap();
    let mut transport = DeviceTransport::new(
        fixture
            .runtime_manifest
            .hands
            .iter()
            .map(|hand| hand.frame_identity().unwrap()),
        DeviceTransportConfig::default(),
    )
    .unwrap();
    let mut writer = SessionWriter::new(Vec::new(), *fixture.session.header()).unwrap();
    let mut reader = fixture.session.reader();
    let mut last_receipt_ns = 0;
    while let Some(record) = reader.next_record().unwrap() {
        let frame = record.decode().unwrap();
        if frame.sequence == 61 {
            assert_eq!(
                live.report().armed_transitions,
                1,
                "the raw-offset boundary occurs after warmup"
            );
        }
        last_receipt_ns = record.host_time_ns;
        dispatch_live_ingress(
            &mut live,
            &mut transport,
            &mut writer,
            &DeviceTransportEvent::Bytes {
                source: frame.hand,
                receipt_time_ns: record.host_time_ns,
                bytes: encode_transport_frame(&frame).unwrap(),
            },
        )
        .unwrap();
        live.tick(last_receipt_ns).unwrap();
    }
    // Release both real merger stages before finish without expiring either hand.
    for delay_ns in [50_000_000, 70_000_000] {
        let now_ns = last_receipt_ns + delay_ns;
        dispatch_live_ingress(
            &mut live,
            &mut transport,
            &mut writer,
            &DeviceTransportEvent::Tick {
                host_time_ns: now_ns,
            },
        )
        .unwrap();
        live.tick(now_ns).unwrap();
    }
    assert_eq!(
        live.report().decoded_frames,
        202,
        "the boundary and all following traffic were consumed"
    );
    assert_eq!(live.report().key_decisions, 2);
    assert_eq!(live.report().terminal_reason, None);
    assert!(live.report().runtime_faults.is_empty());
    let execution = live.finish().unwrap();
    assert_clock_domain_events(execution.output.events(), 999);
    assert!(execution.report.finished_cleanly);
    assert!(execution.report.final_key_state_neutral);
    assert_eq!(
        writer.finish_finalized().unwrap().bytes(),
        fixture.session.bytes()
    );
}

#[test]
fn live_repeated_raw_aggregate_revokes_before_merger_or_evidence_io() {
    use invisible_keyboard::device_transport::{
        DeviceTransport, DeviceTransportConfig, DeviceTransportEvent,
    };
    use invisible_keyboard::transport::encode_transport_frame;

    let fixture = fixture();
    for hand in [Hand::Left, Hand::Right] {
        let observed = SharedRecordingOutput::default();
        let mut live = LiveCalibratedRuntime::new(
            &fixture.calibration_manifest,
            &fixture.runtime_manifest,
            &fixture.artifact,
            confirm_wearables_unmoved(&fixture),
            observed.clone(),
        )
        .unwrap();
        let mut transport = DeviceTransport::new(
            fixture
                .runtime_manifest
                .hands
                .iter()
                .map(|hand| hand.frame_identity().unwrap()),
            DeviceTransportConfig::default(),
        )
        .unwrap();
        let mut writer = SessionWriter::new(Vec::new(), *fixture.session.header()).unwrap();
        let identity = fixture.runtime_manifest.hands[hand as usize]
            .frame_identity()
            .unwrap();
        let config = SyntheticConfig::new(hand, 42)
            .with_noise(0)
            .with_wire_identity(identity.controller_id_sha256, identity.boot_id);
        let mut first = config.idle_window(0).frames()[0];
        first.sequence = 0;
        first.device_time_us = 100_000;
        for sensor in &mut first.sensors {
            sensor.sample_time_offset_us = -1_000;
        }
        let mut repeated = first;
        repeated.sequence = 1;
        for sensor in &mut repeated.sensors {
            sensor.sample_time_offset_us = -999;
        }
        let receipt_ns = 5_100_000_000;
        dispatch_live_ingress(
            &mut live,
            &mut transport,
            &mut writer,
            &DeviceTransportEvent::Bytes {
                source: hand,
                receipt_time_ns: receipt_ns,
                bytes: encode_transport_frame(&first).unwrap(),
            },
        )
        .unwrap();
        let before = live.report().clone();
        let mut closed = false;
        let update = live
            .admit_transport_event(
                &mut transport,
                &DeviceTransportEvent::Bytes {
                    source: hand,
                    receipt_time_ns: receipt_ns + 5_000_000,
                    bytes: encode_transport_frame(&repeated).unwrap(),
                },
                || {
                    assert_eq!(
                        observed.release_all_calls(),
                        2,
                        "neutrality precedes external admission cutoff"
                    );
                    assert_eq!(observed.event_count(), 0);
                    closed = true;
                },
            )
            .unwrap();
        assert!(
            closed,
            "a repeated raw aggregate must revoke before the receipt merger releases it"
        );
        assert!(update.evidence().candidates.is_empty());
        assert_eq!(transport.pending_candidates(), 2);
        let reason = LiveTerminalReason::RuntimeFault(CalibratedRuntimeFault::Pipeline(
            invisible_keyboard::pipeline::PipelineError::InvalidWindow(
                invisible_keyboard::decoder::WindowError::TimeRegression,
            ),
        ));
        assert_eq!(live.report().terminal_reason, Some(reason.clone()));
        assert_eq!(live.report().decisions, before.decisions);
        assert_eq!(live.report().decoded_frames, before.decoded_frames);
        assert!(matches!(
            live.release_transport_update(update, receipt_ns + 5_000_000),
            Err(LiveCalibratedError::TerminalFault(actual)) if actual == reason
        ));
        assert!(matches!(
            live.tick(receipt_ns + 6_000_000),
            Err(LiveCalibratedError::WearSessionRevoked(actual)) if actual == reason
        ));
        let retained = transport
            .retain_evidence(DeviceTransportEvent::Disconnected {
                source: hand,
                receipt_time_ns: receipt_ns + 6_000_000,
            })
            .unwrap();
        append_live_ingress_evidence(&mut writer, &retained);
        let session = writer.finish_finalized().unwrap();
        let mut reader = session.reader();
        for (frame, receipt) in [(first, receipt_ns), (repeated, receipt_ns + 5_000_000)] {
            let record = reader.next_record().unwrap().unwrap();
            assert_eq!(record.host_time_ns, receipt);
            assert_eq!(record.payload, encode_frame(&frame).unwrap());
        }
        assert!(reader.next_record().unwrap().is_none());
        let execution = live.finish().unwrap();
        assert_eq!(execution.report.wear_session_status(), REVOKED_STATUS);
        assert!(execution.report.final_key_state_neutral);
        assert!(execution.report.output_neutrality_confirmed);
    }
}
