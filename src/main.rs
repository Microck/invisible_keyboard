use std::{
    collections::BTreeSet,
    convert::Infallible,
    env,
    error::Error,
    fmt,
    fs::{File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::Path,
    process::ExitCode,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::RecvTimeoutError,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[cfg(target_os = "linux")]
use invisible_keyboard::controller_source::{
    ControllerSourceError, ControllerSourceOperation, Gate0ControlPipeError, Gate0ControlPipes,
    Gate0StopPipeRead, open_controller_source, open_gate0_controller,
};
#[cfg(target_os = "linux")]
use invisible_keyboard::hardware_calibration::{
    HardwareCalibrationConfig, HardwareCalibrationError, HardwareCalibrationResultAcknowledgement,
    HardwareCalibrationRunner,
};
#[cfg(target_os = "linux")]
use invisible_keyboard::labels::list_linux_keyboards;
#[cfg(target_os = "linux")]
use invisible_keyboard::output::{LinuxUinputOutput, probe_core28_uinput, stress_core28_uinput};
use invisible_keyboard::{
    artifact::{ArtifactError, CalibrationArtifact, MAX_ARTIFACT_BYTES},
    calibration_app::{
        CalibrationApp, CalibrationAppError, CalibrationHttpServer, CaptureCalibrationInput,
        CaptureCalibrationRunError, EvidenceMode, run_capture_calibration,
    },
    calibration_capture::{
        CalibrationCaptureError, CalibrationCaptureJournal, MAX_CALIBRATION_CAPTURE_BYTES,
        build_calibration_evidence,
    },
    calibration_engine::{
        CalibrationEngineError, CalibrationFitInput, fit_calibration, validate_calibration,
    },
    clock_epoch::{
        ClockEpochError, ClockEpochFitError, ClockEpochSet, LiveClockReport, LiveClockReportError,
        MAX_CLOCK_EPOCH_SET_BYTES, OperationalControllerClock,
    },
    device_transport::{
        DeviceTransport, DeviceTransportConfig, DeviceTransportConfigurationError,
        DeviceTransportError, DeviceTransportEvent, DeviceTransportFault, DeviceTransportUpdate,
        DeviceWireIdentityBootstrap, Gate0DeviceTransport, Gate0DeviceTransportError,
        Gate0DeviceTransportFault, Gate0DeviceTransportUpdate, IdentityBootstrapConfig,
        IdentityBootstrapError, ObservedQuiesceReceipt,
    },
    evaluation::{SyntheticScenarioReport, run_synthetic_stress_suite},
    gate0::{Gate0Error, score_gate0a},
    gate0_trace::{Gate0CapturePaths, Gate0TraceError, reduce_gate0_capture},
    layout::{CORE28_KEY_COUNT, Hand, PHYSICAL_POSITION_COUNT, RESERVED_POSITION_COUNT, all_slots},
    live::{
        LiveCalibratedError, LiveCalibratedExecution, LiveCalibratedRuntime, LiveTransportUpdate,
        USER_ATTESTED_UNMOVED_STATUS,
    },
    manifest::{
        AcquisitionManifest, AcquisitionManifestTemplate, MAX_AXIS_CHARACTERIZATION_BYTES,
        MAX_MANIFEST_BYTES, MAX_RIG_EVIDENCE_BYTES, ManifestError, ObservedTopologyError,
        format_sha256, sha256,
    },
    neutral::{MAX_NEUTRAL_REPORT_BYTES, NeutralEvidenceError, NeutralEvidenceReport},
    output::{KeyEvent, OutputAdapter, OutputReceipt, ShutdownSignal},
    physical_study::{
        FrozenStudyPipeline, GateDisposition, PHYSICAL_STUDY_ANALYSIS_PLAN_MAX_BYTES,
        PHYSICAL_STUDY_EVIDENCE_JSONL_MAX_BYTES, PHYSICAL_STUDY_EVIDENCE_ROW_MAX_BYTES,
        PHYSICAL_STUDY_SPLIT_MANIFEST_MAX_BYTES, PhysicalStudyAnalysisPlan, PhysicalStudyArtifact,
        PhysicalStudyError, PhysicalStudySplitManifest, analyze_physical_study_jsonl,
        decode_physical_study_evidence_row, decode_physical_study_jsonl,
        encode_physical_study_jsonl, seal_physical_study_evidence_row,
        validate_physical_study_artifact_size,
    },
    pilot::{MAX_PILOT_TRUTH_BYTES, PilotDisposition, PilotError, PilotTruth, reduce_pilot},
    prompts::MAX_SURFACE_PLAN_BYTES,
    protocol::{
        ALL_SENSORS_VALID, QuiesceRequest, SENSOR_COUNT, SensorFrame, SensorReading, encode_frame,
    },
    reader_mailbox::{MailboxReceiver, MailboxSender, bounded_mailbox},
    replay::{CalibratedReplayError, run_calibrated_replay},
    runtime::RuntimeOutputError,
    session::{
        ControllerStream, FinalizedSession, GATE0A_HOST_RESPONSE_TIMEOUT_NS, Gate0aTerminalError,
        MAX_GATE0A_SESSION_BYTES, MAX_SETUP_SESSION_BYTES, SessionError, SessionHeader,
        SessionWriter, audit_gate0a_session_reader, audit_session_reader,
        validate_calibration_session_size, validate_gate0a_session_size, write_reference_session,
    },
    transport::{encode_transport_frame, encode_transport_quiesce_request},
    validation::{
        HeldOutValidation, HeldOutValidationError, MAX_VALIDATION_REPORT_BYTES, ValidationReport,
    },
};
use signal_hook::{
    consts::{SIGINT, SIGTERM},
    flag,
};

enum CliOutput {
    Recording(LiveRecordingOutput),
    #[cfg(target_os = "linux")]
    Linux(Box<LinuxUinputOutput>),
}

/// Bounded live-output sink used when the operator wants evidence without
/// publishing native key events. Unlike the replay adapter, it never retains
/// an unbounded event history that no live caller reads.
#[derive(Default)]
struct LiveRecordingOutput {
    next_ordinal: u64,
}

impl OutputAdapter for LiveRecordingOutput {
    type Error = Infallible;

    fn emit(&mut self, event: KeyEvent) -> Result<OutputReceipt, Self::Error> {
        let ordinal = self.next_ordinal;
        self.next_ordinal = self
            .next_ordinal
            .checked_add(1)
            .expect("live output receipt ordinal does not overflow");
        Ok(OutputReceipt { event, ordinal })
    }

    fn release_all(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl OutputAdapter for CliOutput {
    type Error = io::Error;

    fn emit(&mut self, event: KeyEvent) -> Result<OutputReceipt, Self::Error> {
        match self {
            Self::Recording(output) => match output.emit(event) {
                Ok(receipt) => Ok(receipt),
                Err(never) => match never {},
            },
            #[cfg(target_os = "linux")]
            Self::Linux(output) => output.emit(event),
        }
    }

    fn release_all(&mut self) -> Result<(), Self::Error> {
        match self {
            Self::Recording(output) => match output.release_all() {
                Ok(()) => Ok(()),
                Err(never) => match never {},
            },
            #[cfg(target_os = "linux")]
            Self::Linux(output) => output.release_all(),
        }
    }
}

fn main() -> ExitCode {
    let arguments = env::args().skip(1).collect::<Vec<_>>();
    let mut stdout = io::stdout().lock();
    match run(&arguments, &mut stdout) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(arguments: &[String], output: &mut impl Write) -> Result<(), CliError> {
    let command = arguments.first().map_or("help", String::as_str);
    match command {
        "help" | "--help" | "-h" => {
            require_no_operands(arguments, command)?;
            print_help(output)?;
        }
        "layout" => {
            require_no_operands(arguments, command)?;
            print_layout(output)?;
        }
        "simulate" => {
            require_no_operands(arguments, command)?;
            print_simulation(output)?;
        }
        "frame-fixture" => {
            require_no_operands(arguments, command)?;
            print_frame_fixture(output)?;
        }
        "session-fixture" => write_session_fixture(arguments, output)?,
        "record-stream" => record_stream(arguments, output)?,
        "hash-rig-evidence" => hash_rig_evidence_command(arguments, output)?,
        #[cfg(target_os = "linux")]
        "characterize-axes" => characterize_axes_command(arguments, output)?,
        #[cfg(target_os = "linux")]
        "derive-axes" => derive_axes_command(arguments, output)?,
        "record-hand" => record_hand(arguments, output)?,
        "score-gate0a" => score_gate0a_command(arguments, output)?,
        "score-pilot" => score_pilot_command(arguments, output)?,
        "physical-study-plan" => physical_study_plan(arguments, output)?,
        "physical-study-seal" => physical_study_seal(arguments, output)?,
        "physical-study-score" => physical_study_score(arguments, output)?,
        "verify-session" => verify_session(arguments, output)?,
        "fit-session-clocks" => fit_session_clocks(arguments, output)?,
        "build-calibration-evidence" => build_calibration_evidence_command(arguments, output)?,
        "fit-calibration" => fit_calibration_command(arguments, output)?,
        "validate-calibration" => validate_calibration_command(arguments, output)?,
        "calibrate-replay" => calibrate_replay(arguments, output)?,
        "replay-calibrated" => replay_calibrated(arguments, output)?,
        "run-calibrated" => run_calibrated(arguments, output)?,
        "calibration-demo" => calibration_demo(arguments, output)?,
        #[cfg(target_os = "linux")]
        "calibrate-hardware" => calibrate_hardware(arguments, output)?,
        #[cfg(target_os = "linux")]
        "list-keyboards" => {
            require_no_operands(arguments, command)?;
            print_linux_keyboards(output)?;
        }
        #[cfg(target_os = "linux")]
        "uinput-check" => {
            require_no_operands(arguments, command)?;
            check_uinput(output)?;
        }
        #[cfg(target_os = "linux")]
        "uinput-probe" => {
            require_no_operands(arguments, command)?;
            probe_uinput(output)?;
        }
        #[cfg(target_os = "linux")]
        "uinput-stress" => {
            require_no_operands(arguments, command)?;
            stress_uinput(output)?;
        }
        unknown => return Err(CliError::UnknownCommand(unknown.to_owned())),
    }
    Ok(())
}

fn print_help(output: &mut impl Write) -> io::Result<()> {
    writeln!(output, "invisible-keyboard reference stack")?;
    writeln!(output)?;
    writeln!(output, "Usage: invisible-keyboard <command>")?;
    writeln!(output)?;
    writeln!(output, "Commands:")?;
    writeln!(
        output,
        "  hash-rig-evidence <rig-evidence>  print the typed canonical rig hash without authorizing capture"
    )?;
    #[cfg(target_os = "linux")]
    {
        writeln!(
            output,
            "  characterize-axes <template> <rig-evidence> <output-directory> <device> [<device>]  guide non-authorizing six-face raw capture"
        )?;
        writeln!(
            output,
            "  derive-axes <template> <rig-evidence> <axis-session> <output-directory>  derive the same axis evidence from raw replay"
        )?;
    }
    writeln!(
        output,
        "  layout         print the physical 34-position/Core-28 manifest"
    )?;
    writeln!(
        output,
        "  simulate       run the fixed-seed synthetic stress suite"
    )?;
    writeln!(
        output,
        "  frame-fixture  print the canonical zero-frame fixture"
    )?;
    writeln!(
        output,
        "  session-fixture <manifest> <path>  write the deterministic two-hand session"
    )?;
    writeln!(
        output,
        "  record-stream <template> <rig-evidence> <axis-characterization> <device> [<device>] <locked-manifest> <session>  discover boots and capture one or two source-bound COBS streams in template order"
    )?;
    writeln!(
        output,
        "  record-hand <template> <rig-evidence> <axis-characterization> <device> <locked-manifest.partial> <session.partial> --ready-fd <fd> --stop-fd <fd>  run the one-TTY Gate 0A recorder"
    )?;
    writeln!(
        output,
        "  score-gate0a <manifest> <rig-evidence> <axis-characterization> <session> <raw-capture> <capture-sidecar> <report-output>  reduce and score one-hand Gate 0A evidence"
    )?;
    writeln!(
        output,
        "  score-pilot <manifest> <rig-evidence> <axis-characterization> <session> <truth.json> <truth-artifact> <clock-mapping> <report-output>  score a non-armable one-hand diagnostic pilot"
    )?;
    writeln!(
        output,
        "  physical-study-plan <roster.jsonl> <manifest.json> <plan.json>  freeze one physical-study round before collection"
    )?;
    writeln!(
        output,
        "  physical-study-seal <row.json> <evidence.jsonl>  seal and append one complete physical-study row"
    )?;
    writeln!(
        output,
        "  physical-study-score <evidence.jsonl> <manifest.json> <plan.json> <report.json>  validate and score one frozen physical-study round"
    )?;
    writeln!(
        output,
        "  verify-session <manifest> <rig-evidence> <axis-characterization> <path>  verify manifest evidence and replay a session"
    )?;
    writeln!(
        output,
        "  fit-session-clocks <manifest> <rig-evidence> <axis-characterization> <session> <output>  fit bound controller clocks"
    )?;
    writeln!(
        output,
        "  build-calibration-evidence <manifest> <rig-evidence> <axis-characterization> <clocks> <session> <capture> <surface-plan> <evidence-output> <matching-rule-output>"
    )?;
    writeln!(
        output,
        "  fit-calibration <manifest> <rig-evidence> <axis-characterization> <clocks> <capture> <surface-plan> <session> <neutral-report> <artifact-output>"
    )?;
    writeln!(
        output,
        "  validate-calibration <manifest> <rig-evidence> <axis-characterization> <clocks> <artifact> <capture> <surface-plan> <session> <report-output>"
    )?;
    writeln!(
        output,
        "  calibrate-replay <manifest> <rig-evidence> <axis-characterization> <fit-clocks> <fit-capture> <fit-surface-plan> <fit-session> <neutral-report> <heldout-clocks> <heldout-capture> <heldout-surface-plan> <heldout-session> <output-directory>"
    )?;
    writeln!(
        output,
        "  replay-calibrated <cal-manifest> <rig-evidence> <axis-characterization> <run-manifest> <clocks> <artifact> <calibration-output-directory> <session>"
    )?;
    writeln!(
        output,
        "  run-calibrated <cal-manifest> <rig-evidence> <axis-characterization> <runtime-template> <artifact> <calibration-output-directory> <left-device> <right-device> <runtime-manifest> <session> <live-clock-report> [recording|uinput] --confirm-wearables-unmoved"
    )?;
    writeln!(
        output,
        "  calibration-demo [port]  run the local replay-only calibration UI"
    )?;
    #[cfg(target_os = "linux")]
    writeln!(
        output,
        "  calibrate-hardware <template> <rig-evidence> <axis-characterization> <left-device> <right-device> <keyboard-device> <output-directory> [port]  run source-bound guided hardware calibration"
    )?;
    #[cfg(target_os = "linux")]
    writeln!(
        output,
        "  list-keyboards  list readable EV_KEY calibration keyboards"
    )?;
    #[cfg(target_os = "linux")]
    writeln!(
        output,
        "  uinput-check   create and release a Core-28 virtual keyboard"
    )?;
    #[cfg(target_os = "linux")]
    writeln!(
        output,
        "  uinput-probe   exclusively grab and verify one kernel key pair"
    )?;
    #[cfg(target_os = "linux")]
    writeln!(
        output,
        "  uinput-stress  exclusively grab and stress kernel output safety"
    )?;
    writeln!(output, "  help           print this help")
}

fn print_layout(output: &mut impl Write) -> io::Result<()> {
    writeln!(output, "physical_positions={PHYSICAL_POSITION_COUNT}")?;
    writeln!(output, "core28_enabled={CORE28_KEY_COUNT}")?;
    writeln!(output, "reserved_positions={RESERVED_POSITION_COUNT}")?;
    for (index, slot) in all_slots().iter().enumerate() {
        let usage = slot.usage.map_or("reserved", |_| slot.label);
        writeln!(
            output,
            "slot {index:02} hand={:?} finger={:?} finger_slot={} usage={usage}",
            slot.position.hand, slot.position.finger, slot.position.slot
        )?;
    }
    Ok(())
}

fn print_simulation(output: &mut impl Write) -> Result<(), CliError> {
    let report =
        run_synthetic_stress_suite().map_err(|error| CliError::Simulation(error.to_string()))?;
    writeln!(output, "evidence={}", report.evidence_label)?;
    writeln!(output, "limitation={}", report.limitations)?;
    print_scenario(output, &report.nominal)?;
    for scenario in &report.scenarios {
        print_scenario(output, scenario)?;
    }
    for breakpoint in &report.breakpoints {
        writeln!(
            output,
            "breakpoint dimension={} unit={} levels_tested={} last_zero={:?} first_failure={:?} failures_at_first={}",
            breakpoint.dimension,
            breakpoint.unit,
            breakpoint.levels_tested,
            breakpoint.last_zero_failure_level,
            breakpoint.first_failure_level,
            breakpoint.failures_at_first_level
        )?;
    }
    writeln!(
        output,
        "ordinary_motion exposure_us={} false_activations={}",
        report.ordinary_motion_exposure_us, report.ordinary_motion_false_activations
    )?;
    writeln!(
        output,
        "ordinary_motion zero_event_upper_95_milli_per_hour={:?}",
        report.ordinary_motion_zero_event_upper_95_milli_per_hour
    )?;
    writeln!(
        output,
        "simultaneous windows={} abstentions={}",
        report.simultaneous_windows, report.simultaneous_abstentions
    )?;
    writeln!(
        output,
        "dropout windows={} abstentions={}",
        report.dropout_windows, report.dropout_abstentions
    )?;
    writeln!(
        output,
        "whole_hand_dropout windows={} abstentions={}",
        report.whole_hand_dropout_windows, report.whole_hand_dropout_abstentions
    )?;
    Ok(())
}

fn print_scenario(output: &mut impl Write, report: &SyntheticScenarioReport) -> io::Result<()> {
    writeln!(
        output,
        "scenario={} positions={} position_errors={} key_errors={} abstentions={} reserved_rejections={} false_activations={}",
        report.name,
        report.intentional_positions,
        report.position_errors,
        report.key_errors,
        report.abstentions,
        report.reserved_rejections,
        report.false_activations
    )
}

fn print_frame_fixture(output: &mut impl Write) -> io::Result<()> {
    let frame = SensorFrame {
        hand: Hand::Left,
        controller_id_sha256: [0x11; 32],
        boot_id: [0x22; 16],
        flags: 0,
        valid_sensor_mask: ALL_SENSORS_VALID,
        sequence: 0,
        device_time_us: 0,
        sensors: [SensorReading::default(); SENSOR_COUNT],
    };
    let bytes = encode_frame(&frame).expect("canonical fixture frame is valid");
    writeln!(output, "frame_length={}", bytes.len())?;
    write!(output, "frame_hex=")?;
    for byte in bytes {
        write!(output, "{byte:02x}")?;
    }
    writeln!(output)?;

    let transport =
        encode_transport_frame(&frame).expect("canonical fixture transport frame is valid");
    writeln!(output, "transport_length={}", transport.len())?;
    write!(output, "transport_hex=")?;
    for byte in transport {
        write!(output, "{byte:02x}")?;
    }
    writeln!(output)
}

fn write_session_fixture(arguments: &[String], output: &mut impl Write) -> Result<(), CliError> {
    let (manifest_path, path) = required_paths(arguments, "session-fixture")?;
    let manifest = load_structural_manifest(manifest_path)?;
    if manifest.hands.len() != 2 {
        return Err(CliError::Usage(
            "session-fixture writes a two-hand stream and requires a two-hand manifest".to_owned(),
        ));
    }
    let manifest_sha256 = manifest.sha256()?;
    let file = create_session_file(path)?;
    let (writer, summary) = write_reference_session(
        io::BufWriter::new(file),
        manifest_sha256,
        manifest.nominal_period_us,
        manifest
            .frame_identities()?
            .try_into()
            .expect("two-hand manifest yields two wire identities"),
    )?;
    sync_file_and_parent_directory(writer.get_ref(), Path::new(path))?;
    writeln!(output, "session_fixture=written")?;
    writeln!(output, "manifest={manifest_path}")?;
    writeln!(
        output,
        "manifest_sha256={}",
        format_sha256(&manifest_sha256)
    )?;
    writeln!(output, "path={path}")?;
    writeln!(output, "records={}", summary.total_records)?;
    Ok(())
}

fn record_stream(arguments: &[String], output: &mut impl Write) -> Result<(), CliError> {
    if !(7..=8).contains(&arguments.len()) {
        return Err(CliError::Usage(
            "record-stream requires template, rig evidence, axis characterization, one or two devices in template order, locked manifest, and session output paths"
                .to_owned(),
        ));
    }
    let template_path = &arguments[1];
    let evidence_paths = [arguments[2].as_str(), arguments[3].as_str()];
    let output_index = arguments.len() - 2;
    let device_arguments = &arguments[4..output_index];
    let mut device_paths = [""; 2];
    for (path, argument) in device_paths.iter_mut().zip(device_arguments) {
        *path = argument.as_str();
    }
    let device_paths = &device_paths[..device_arguments.len()];
    let locked_manifest_path = &arguments[output_index];
    let session_path = &arguments[output_index + 1];
    if device_paths.len() == 2
        && (device_paths[0] == device_paths[1]
            || std::fs::canonicalize(device_paths[0])? == std::fs::canonicalize(device_paths[1])?)
    {
        return Err(CliError::Usage(
            "record-stream device paths must refer to distinct sources".to_owned(),
        ));
    }
    validate_capture_paths(
        "record-stream",
        template_path,
        evidence_paths,
        device_paths,
        locked_manifest_path,
        session_path,
    )?;
    let evidence = ManifestEvidenceLoader::load(&arguments[2], &arguments[3])?;
    let template = evidence.load_template(template_path)?;
    let controllers = template.expected_controllers();
    if controllers.len() != device_paths.len() {
        return Err(CliError::Usage(format!(
            "record-stream requires {} device(s) in template controller order; received {}",
            controllers.len(),
            device_paths.len(),
        )));
    }
    let mut sources = [(Hand::Left, ""); 2];
    for ((source, (hand, _)), path) in sources.iter_mut().zip(controllers).zip(device_paths) {
        *source = (hand, *path);
    }
    record_sources(
        template_path,
        template,
        &sources[..device_paths.len()],
        locked_manifest_path,
        session_path,
        output,
    )
}

fn record_hand(arguments: &[String], output: &mut impl Write) -> Result<(), CliError> {
    let operands = RecordHandOperands::parse(arguments)?;
    let template_path = operands.template_path;
    let evidence_paths = [
        operands.rig_evidence_path,
        operands.axis_characterization_path,
    ];
    let device_path = operands.device_path;
    let locked_manifest_path = operands.locked_manifest_path;
    let session_path = operands.session_path;
    validate_capture_paths(
        "record-hand",
        template_path,
        evidence_paths,
        &[device_path],
        locked_manifest_path,
        session_path,
    )?;
    let evidence = ManifestEvidenceLoader::load(
        operands.rig_evidence_path,
        operands.axis_characterization_path,
    )?;
    let template = evidence.load_template(template_path)?;
    let expected = template.expected_controllers();
    if expected.len() != 1 {
        return Err(CliError::Usage(
            "record-hand requires a one-hand acquisition template".to_owned(),
        ));
    }

    #[cfg(target_os = "linux")]
    return record_gate0_hand(&operands, template, expected[0], output);
    #[cfg(not(target_os = "linux"))]
    {
        let _ = output;
        Err(CliError::Usage(
            "Gate 0A recording is currently supported only on Linux".to_owned(),
        ))
    }
}

struct RecordHandOperands<'a> {
    template_path: &'a str,
    rig_evidence_path: &'a str,
    axis_characterization_path: &'a str,
    device_path: &'a str,
    locked_manifest_path: &'a str,
    session_path: &'a str,
    ready_descriptor: i32,
    stop_descriptor: i32,
}

impl<'a> RecordHandOperands<'a> {
    fn parse(arguments: &'a [String]) -> Result<Self, CliError> {
        if arguments.len() != 11 || arguments[7] != "--ready-fd" || arguments[9] != "--stop-fd" {
            return Err(CliError::Usage(
                "record-hand requires template, rig evidence, axis characterization, device, locked manifest partial, session partial, --ready-fd, and --stop-fd operands"
                    .to_owned(),
            ));
        }
        let parse_descriptor = |value: &str, flag: &str| {
            let descriptor = value.parse::<i32>().map_err(|_| {
                CliError::Usage(format!(
                    "record-hand {flag} must be a nonnegative integer file descriptor"
                ))
            })?;
            if descriptor < 0 {
                return Err(CliError::Usage(format!(
                    "record-hand {flag} must be a nonnegative integer file descriptor"
                )));
            }
            Ok(descriptor)
        };
        Ok(Self {
            template_path: &arguments[1],
            rig_evidence_path: &arguments[2],
            axis_characterization_path: &arguments[3],
            device_path: &arguments[4],
            locked_manifest_path: &arguments[5],
            session_path: &arguments[6],
            ready_descriptor: parse_descriptor(&arguments[8], "--ready-fd")?,
            stop_descriptor: parse_descriptor(&arguments[10], "--stop-fd")?,
        })
    }
}

fn validate_capture_paths(
    command: &str,
    template_path: &str,
    evidence_paths: [&str; 2],
    device_paths: &[&str],
    locked_manifest_path: &str,
    session_path: &str,
) -> Result<(), CliError> {
    if locked_manifest_path == session_path {
        return Err(CliError::Usage(format!(
            "{command} locked manifest and session output paths must differ"
        )));
    }
    let protected_inputs = [template_path, evidence_paths[0], evidence_paths[1]];
    if protected_inputs.contains(&locked_manifest_path) || protected_inputs.contains(&session_path)
    {
        return Err(CliError::Usage(format!(
            "{command} outputs must not replace the acquisition template or physical evidence"
        )));
    }
    if device_paths
        .iter()
        .any(|device| [locked_manifest_path, session_path].contains(device))
    {
        return Err(CliError::Usage(format!(
            "{command} outputs must not replace a controller device"
        )));
    }
    ensure_output_absent(locked_manifest_path)?;
    ensure_output_absent(session_path)?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn open_physical_controller(path: &str) -> Result<File, CliError> {
    Ok(open_controller_source(path)?.into_file())
}

#[cfg(not(target_os = "linux"))]
fn open_physical_controller(_path: &str) -> Result<File, CliError> {
    Err(CliError::Usage(
        "physical controller capture is currently supported only on Linux".to_owned(),
    ))
}

#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
fn record_gate0_hand(
    operands: &RecordHandOperands<'_>,
    template: AcquisitionManifestTemplate,
    expected_controller: (Hand, [u8; 32]),
    output: &mut impl Write,
) -> Result<(), CliError> {
    let hand = expected_controller.0;
    let signal = capture_signal_flag()?;
    // Native USB disappears while reset is held for the analyzer's boot prefix.
    // This wait applies only before the first open, never after a disconnect.
    let enumeration_deadline = Instant::now() + Duration::from_secs(3);
    let controller = loop {
        reject_gate0_signal(&signal)?;
        match open_gate0_controller(operands.device_path) {
            Ok(controller) => break controller,
            Err(error)
                if matches!(
                    &error,
                    ControllerSourceError::Io {
                        operation: ControllerSourceOperation::Open,
                        source,
                        ..
                    } if source.kind() == io::ErrorKind::NotFound
                ) && Instant::now() < enumeration_deadline =>
            {
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => return Err(error.into()),
        }
    };
    // SAFETY: the wrapper passes ownership of two distinct inherited pipe
    // ends. Argument parsing rejects non-integers; the constructor validates
    // descriptor kind, direction, aliasing, and standard-descriptor reuse.
    let mut control = unsafe {
        Gate0ControlPipes::from_inherited_descriptors(
            operands.ready_descriptor,
            operands.stop_descriptor,
        )
    }?;
    reject_pre_ready_control(&mut control)?;

    let capture_start = Instant::now();
    let mut controller_writer = controller.writer;
    let input = spawn_device_readers(
        [(hand, controller.reader)],
        Arc::clone(&signal),
        capture_start,
    );
    let mut bootstrap = DeviceWireIdentityBootstrap::new(
        [expected_controller],
        IdentityBootstrapConfig::default(),
    )?;
    while !bootstrap.is_complete() {
        reject_pre_ready_control(&mut control)?;
        reject_gate0_signal(&signal)?;
        match input.recv_timeout(Duration::from_millis(10)) {
            Ok(CaptureInput::Chunk {
                source,
                receipt_time_ns,
                bytes,
            }) => bootstrap.handle(DeviceTransportEvent::Bytes {
                source,
                receipt_time_ns,
                bytes,
            })?,
            Ok(CaptureInput::End { source, .. }) | Ok(CaptureInput::Failed { source, .. }) => {
                return Err(Gate0RecorderFailure::SourceDisconnected(source).into());
            }
            Err(RecvTimeoutError::Disconnected) => {
                return Err(Gate0RecorderFailure::SourceDisconnected(hand).into());
            }
            Err(RecvTimeoutError::Timeout) => {
                bootstrap.handle(DeviceTransportEvent::Tick {
                    host_time_ns: elapsed_ns(capture_start),
                })?;
            }
        }
    }

    let bootstrapped = bootstrap.finish()?;
    let identity = *bootstrapped
        .identities
        .first()
        .expect("one-hand bootstrap returns one identity");
    let manifest = template.lock(&bootstrapped.identities)?;
    let manifest_bytes = manifest.canonical_json()?;
    let manifest_sha256 = sha256(&manifest_bytes);
    let session_id = new_session_id();
    let header = SessionHeader {
        nominal_period_us: manifest.nominal_period_us,
        manifest_sha256,
        session_id,
    };
    let mut session = create_capture_session(
        operands.locked_manifest_path,
        &manifest_bytes,
        operands.session_path,
        header,
    )?;
    let mut transport = Gate0DeviceTransport::new(identity)?;
    let mut last_receipt_time_ns = 0_u64;
    for event in bootstrapped.events {
        let DeviceTransportEvent::Bytes {
            receipt_time_ns,
            bytes,
            ..
        } = event
        else {
            unreachable!("bootstrap retains only byte events")
        };
        last_receipt_time_ns = receipt_time_ns;
        let update = transport.push(receipt_time_ns, &bytes)?;
        append_gate0_transport_update(&mut session, hand, update)?;
    }

    // Readiness means the identity is locked and an IKS3 header is observable
    // on disk. No footer exists, and no stop byte has been accepted yet.
    session.flush()?;
    reject_pre_ready_control(&mut control)?;
    reject_gate0_signal(&signal)?;
    control.publish_ready()?;

    let (request, request_completion) = loop {
        reject_gate0_signal(&signal)?;
        match control.read_stop()? {
            Gate0StopPipeRead::Bytes(bytes) if bytes == [1] => {
                session.flush()?;
                let request_start_time_ns = elapsed_ns(capture_start);
                let request = QuiesceRequest {
                    hand,
                    controller_id_sha256: identity.controller_id_sha256,
                    boot_id: identity.boot_id,
                    session_id,
                    request_start_time_ns,
                };
                let encoded =
                    encode_transport_quiesce_request(&request).map_err(SessionError::Quiesce)?;
                transport.begin_request(request)?;
                session.append_quiesce_request(
                    ControllerStream::from(hand),
                    &request,
                    request_start_time_ns,
                )?;
                session.flush()?;
                let (completion, request_completion) = std::sync::mpsc::sync_channel(1);
                // A blocked CDC write or tcdrain must not own the supervisor.
                // On failure the CLI exits without joining this worker, just
                // as it does for a controller reader blocked in read().
                thread::spawn(move || {
                    let outcome = controller_writer
                        .write_all(&encoded)
                        .map_err(CliError::Io)
                        .and_then(|()| {
                            controller_writer
                                .drain_output()
                                .map_err(CliError::ControllerSource)
                        });
                    let _ = completion.send((elapsed_ns(capture_start), outcome));
                });
                break (request, request_completion);
            }
            Gate0StopPipeRead::Bytes(bytes) => {
                return Err(Gate0RecorderFailure::InvalidStopBytes(bytes).into());
            }
            Gate0StopPipeRead::Eof => {
                return Err(Gate0RecorderFailure::GuardClosedBeforeReceipt.into());
            }
            Gate0StopPipeRead::WouldBlock => {}
        }

        match input.recv_timeout(Duration::from_millis(10)) {
            Ok(CaptureInput::Chunk {
                receipt_time_ns,
                bytes,
                ..
            }) => {
                last_receipt_time_ns = receipt_time_ns;
                let update = transport.push(receipt_time_ns, &bytes)?;
                append_gate0_transport_update(&mut session, hand, update)?;
            }
            Ok(CaptureInput::End { source, .. }) | Ok(CaptureInput::Failed { source, .. }) => {
                return Err(Gate0RecorderFailure::SourceDisconnected(source).into());
            }
            Err(RecvTimeoutError::Disconnected) => {
                return Err(Gate0RecorderFailure::SourceDisconnected(hand).into());
            }
            Err(RecvTimeoutError::Timeout) => {
                if elapsed_ns(capture_start).saturating_sub(last_receipt_time_ns)
                    > DeviceTransportConfig::default().source_silence_timeout_ns
                {
                    return Err(Gate0RecorderFailure::SourceSilent(hand).into());
                }
            }
        }
    };

    let mut request_drained = false;
    let mut pending_receipt = None;
    let observed_receipt = loop {
        reject_gate0_signal(&signal)?;
        match control.read_stop()? {
            Gate0StopPipeRead::Bytes(bytes) => {
                return Err(Gate0RecorderFailure::DuplicateStopBytes(bytes).into());
            }
            Gate0StopPipeRead::Eof if pending_receipt.is_none() => {
                return Err(Gate0RecorderFailure::GuardClosedBeforeReceipt.into());
            }
            Gate0StopPipeRead::WouldBlock | Gate0StopPipeRead::Eof => {}
        }

        match input.recv_timeout(Duration::from_millis(10)) {
            Ok(CaptureInput::Chunk {
                receipt_time_ns,
                bytes,
                ..
            }) => {
                let update = transport.push(receipt_time_ns, &bytes)?;
                if let Some(receipt) = append_gate0_transport_update(&mut session, hand, update)? {
                    if receipt
                        .observation_time_ns
                        .saturating_sub(request.request_start_time_ns)
                        > GATE0A_HOST_RESPONSE_TIMEOUT_NS
                    {
                        return Err(Gate0RecorderFailure::ReceiptTimeout {
                            actual_ns: receipt
                                .observation_time_ns
                                .saturating_sub(request.request_start_time_ns),
                        }
                        .into());
                    }
                    session.flush()?;
                    pending_receipt = Some(receipt);
                }
            }
            Ok(CaptureInput::End { source, .. }) | Ok(CaptureInput::Failed { source, .. }) => {
                return Err(Gate0RecorderFailure::SourceDisconnected(source).into());
            }
            Err(RecvTimeoutError::Disconnected) => {
                return Err(Gate0RecorderFailure::SourceDisconnected(hand).into());
            }
            Err(RecvTimeoutError::Timeout) => {}
        }
        if !request_drained {
            match request_completion.try_recv() {
                Ok((completed_at_ns, outcome)) => {
                    outcome?;
                    let actual_ns = completed_at_ns.saturating_sub(request.request_start_time_ns);
                    if actual_ns > GATE0A_HOST_RESPONSE_TIMEOUT_NS {
                        return Err(Gate0RecorderFailure::RequestTimeout { actual_ns }.into());
                    }
                    request_drained = true;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    return Err(io::Error::other(
                        "Gate 0A request writer ended without drain confirmation",
                    )
                    .into());
                }
            }
        }
        if request_drained && let Some(receipt) = pending_receipt {
            break receipt;
        }
        let actual_ns = elapsed_ns(capture_start).saturating_sub(request.request_start_time_ns);
        if actual_ns > GATE0A_HOST_RESPONSE_TIMEOUT_NS {
            return Err(if request_drained {
                Gate0RecorderFailure::ReceiptTimeout { actual_ns }
            } else {
                Gate0RecorderFailure::RequestTimeout { actual_ns }
            }
            .into());
        }
    };

    wait_for_analyzer_guard(
        &mut control,
        &input,
        &signal,
        &mut session,
        &mut transport,
        hand,
    )?;
    observe_final_tty_silence(
        &mut control,
        &input,
        &signal,
        &mut session,
        &mut transport,
        hand,
    )?;
    // Signal handlers and this compare-exchange modify the same atomic. The
    // winner therefore defines one exact boundary: a signal stored first
    // aborts without END3, while a commit stored first cannot be revoked by a
    // later signal after capture has completed.
    let footer_commit = Gate0FooterCommit::claim(&signal)?;
    let (writer, summary) = footer_commit.finish(session)?;
    sync_file_and_parent_directory(writer.get_ref(), Path::new(operands.session_path))?;
    let audit = audit_gate0a_session_reader(File::open(operands.session_path)?)?;
    debug_assert_eq!(
        audit.terminal().receipt,
        observed_receipt.receipt,
        "strict audit returns the receipt admitted by the live dispatcher"
    );

    writeln!(output, "evidence=recorded")?;
    writeln!(output, "controller_identity=locked")?;
    writeln!(output, "deterministic_quiesce=host-confirmed")?;
    writeln!(output, "template={}", operands.template_path)?;
    writeln!(output, "manifest={}", operands.locked_manifest_path)?;
    writeln!(
        output,
        "{}_device={}",
        hand_name(hand),
        operands.device_path
    )?;
    writeln!(
        output,
        "manifest_sha256={}",
        format_sha256(&manifest_sha256)
    )?;
    writeln!(output, "path={}", operands.session_path)?;
    writeln!(output, "total_records={}", summary.total_records)?;
    writeln!(output, "invalid_records={}", summary.invalid_records)?;
    writeln!(output, "quiesce_requests={}", summary.quiesce_requests)?;
    writeln!(output, "quiesce_receipts={}", summary.quiesce_receipts)?;
    writeln!(
        output,
        "receipt_response_time_ns={}",
        observed_receipt
            .observation_time_ns
            .saturating_sub(request.request_start_time_ns)
    )?;
    writeln!(
        output,
        "final_sequence={}",
        observed_receipt.receipt.final_sequence
    )?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn reject_pre_ready_control(control: &mut Gate0ControlPipes) -> Result<(), CliError> {
    match control.read_stop()? {
        Gate0StopPipeRead::WouldBlock => Ok(()),
        Gate0StopPipeRead::Bytes(bytes) => Err(Gate0RecorderFailure::StopBeforeReady(bytes).into()),
        Gate0StopPipeRead::Eof => Err(Gate0RecorderFailure::GuardClosedBeforeReady.into()),
    }
}

#[cfg(target_os = "linux")]
fn reject_gate0_signal(signal: &AtomicUsize) -> Result<(), CliError> {
    if let Some(signal) = pending_shutdown_signal(signal) {
        return Err(Gate0RecorderFailure::Signal(signal).into());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
#[must_use = "claiming the Gate 0 footer boundary must be followed by finishing the session"]
#[derive(Debug)]
struct Gate0FooterCommit;

#[cfg(target_os = "linux")]
impl Gate0FooterCommit {
    fn claim(signal: &AtomicUsize) -> Result<Self, CliError> {
        if signal
            .compare_exchange(
                0,
                GATE0_FOOTER_COMMITTED,
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .is_err()
        {
            reject_gate0_signal(signal)?;
            unreachable!("the Gate 0 footer commit transition occurs exactly once");
        }
        Ok(Self)
    }

    fn finish<W: Write>(
        self,
        session: SessionWriter<W>,
    ) -> Result<(W, invisible_keyboard::session::SessionSummary), SessionError> {
        session.finish()
    }
}

#[cfg(target_os = "linux")]
fn append_gate0_transport_update<W: Write>(
    session: &mut SessionWriter<W>,
    hand: Hand,
    update: Gate0DeviceTransportUpdate,
) -> Result<Option<ObservedQuiesceReceipt>, CliError> {
    for candidate in &update.candidates {
        session.append_transport_candidate(
            ControllerStream::from(hand),
            &candidate.candidate,
            candidate.receipt_time_ns,
        )?;
    }
    if let Some(observed) = update.receipt {
        session.append_quiesce_receipt(
            ControllerStream::from(hand),
            &observed.receipt,
            observed.observation_time_ns,
        )?;
    }
    if let Some(fault) = update.fault {
        return Err(CliError::Gate0DeviceTransportFault(fault));
    }
    Ok(update.receipt)
}

#[cfg(target_os = "linux")]
fn wait_for_analyzer_guard<W: Write>(
    control: &mut Gate0ControlPipes,
    input: &DeviceReaders,
    signal: &AtomicUsize,
    session: &mut SessionWriter<W>,
    transport: &mut Gate0DeviceTransport,
    hand: Hand,
) -> Result<(), CliError> {
    loop {
        reject_gate0_signal(signal)?;
        match control.read_stop()? {
            Gate0StopPipeRead::Bytes(bytes) => {
                return Err(Gate0RecorderFailure::DuplicateStopBytes(bytes).into());
            }
            Gate0StopPipeRead::Eof => return Ok(()),
            Gate0StopPipeRead::WouldBlock => {}
        }
        reject_post_receipt_device_event(
            input.recv_timeout(Duration::from_millis(10)),
            session,
            transport,
            hand,
        )?;
    }
}

#[cfg(target_os = "linux")]
fn observe_final_tty_silence<W: Write>(
    control: &mut Gate0ControlPipes,
    input: &DeviceReaders,
    signal: &AtomicUsize,
    session: &mut SessionWriter<W>,
    transport: &mut Gate0DeviceTransport,
    hand: Hand,
) -> Result<(), CliError> {
    let deadline = Instant::now() + Duration::from_millis(100);
    while Instant::now() < deadline {
        reject_gate0_signal(signal)?;
        match control.read_stop()? {
            Gate0StopPipeRead::Eof | Gate0StopPipeRead::WouldBlock => {}
            Gate0StopPipeRead::Bytes(bytes) => {
                return Err(Gate0RecorderFailure::DuplicateStopBytes(bytes).into());
            }
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        reject_post_receipt_device_event(
            input.recv_timeout(remaining.min(Duration::from_millis(10))),
            session,
            transport,
            hand,
        )?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn reject_post_receipt_device_event<W: Write>(
    event: Result<CaptureInput, RecvTimeoutError>,
    session: &mut SessionWriter<W>,
    transport: &mut Gate0DeviceTransport,
    hand: Hand,
) -> Result<(), CliError> {
    match event {
        Ok(CaptureInput::Chunk {
            receipt_time_ns,
            bytes,
            ..
        }) => {
            let update = transport.push(receipt_time_ns, &bytes)?;
            append_gate0_transport_update(session, hand, update)?;
            Ok(())
        }
        Ok(CaptureInput::End { source, .. }) | Ok(CaptureInput::Failed { source, .. }) => {
            Err(Gate0RecorderFailure::SourceDisconnected(source).into())
        }
        Err(RecvTimeoutError::Disconnected) => {
            Err(Gate0RecorderFailure::SourceDisconnected(hand).into())
        }
        Err(RecvTimeoutError::Timeout) => Ok(()),
    }
}

fn record_sources(
    template_path: &str,
    template: AcquisitionManifestTemplate,
    sources: &[(Hand, &str)],
    locked_manifest_path: &str,
    session_path: &str,
    output: &mut impl Write,
) -> Result<(), CliError> {
    let signal = capture_signal_flag()?;
    let opened_sources = sources
        .iter()
        .map(|(hand, path)| open_physical_controller(path).map(|file| (*hand, file)))
        .collect::<Result<Vec<_>, _>>()?;
    let capture_start = Instant::now();
    let input = spawn_device_readers(opened_sources, Arc::clone(&signal), capture_start);
    let mut bootstrap = DeviceWireIdentityBootstrap::new(
        template.expected_controllers(),
        IdentityBootstrapConfig::default(),
    )?;
    let mut deferred_disconnects = Vec::new();

    while !bootstrap.is_complete() {
        match input.recv_timeout(Duration::from_millis(50)) {
            Ok(CaptureInput::Chunk {
                source,
                receipt_time_ns,
                bytes,
            }) => bootstrap.handle(DeviceTransportEvent::Bytes {
                source,
                receipt_time_ns,
                bytes,
            })?,
            Ok(CaptureInput::End {
                source,
                receipt_time_ns,
            }) => {
                // A finite source can enqueue EOF before another reader's
                // identity-bearing chunk. Apply each disconnect only after
                // every declared identity locks and the session exists.
                deferred_disconnects.push((source, receipt_time_ns));
            }
            Ok(CaptureInput::Failed { error, .. }) => return Err(CliError::Io(error)),
            Err(RecvTimeoutError::Disconnected) => {
                return Err(CliError::IdentityBootstrap(
                    IdentityBootstrapError::Incomplete,
                ));
            }
            Err(RecvTimeoutError::Timeout) if signal.load(Ordering::SeqCst) != 0 => {
                return Err(CliError::IdentityBootstrap(
                    IdentityBootstrapError::Incomplete,
                ));
            }
            Err(RecvTimeoutError::Timeout) => bootstrap.handle(DeviceTransportEvent::Tick {
                host_time_ns: elapsed_ns(capture_start),
            })?,
        }
    }

    let bootstrapped = bootstrap.finish()?;
    let declared_hands = bootstrapped
        .identities
        .iter()
        .map(|identity| identity.hand)
        .collect::<Vec<_>>();
    let manifest = template.lock(&bootstrapped.identities)?;
    let manifest_sha256 = manifest.sha256()?;
    let header = SessionHeader {
        nominal_period_us: manifest.nominal_period_us,
        manifest_sha256,
        session_id: new_session_id(),
    };
    let mut session = create_capture_session(
        locked_manifest_path,
        &manifest.canonical_json()?,
        session_path,
        header,
    )?;
    let mut transport =
        DeviceTransport::new(bootstrapped.identities, DeviceTransportConfig::default())?;
    let mut connected = declared_hands.iter().copied().collect::<BTreeSet<_>>();
    let mut terminal_error = None;

    for event in bootstrapped.events {
        let update = transport.handle(&event)?;
        append_device_transport_update(&mut session, &update)?;
        if let Some(fault) = first_fatal_capture_fault(&update) {
            terminal_error = Some(CliError::DeviceTransportFault(fault));
            break;
        }
    }

    for (source, receipt_time_ns) in deferred_disconnects {
        let update = transport.handle(&DeviceTransportEvent::Disconnected {
            source,
            receipt_time_ns,
        })?;
        append_device_transport_update(&mut session, &update)?;
        connected.remove(&source);
    }

    while terminal_error.is_none() && !connected.is_empty() {
        match input.recv_timeout(Duration::from_millis(50)) {
            Ok(CaptureInput::Chunk {
                source,
                receipt_time_ns,
                bytes,
            }) => {
                let update = transport.handle(&DeviceTransportEvent::Bytes {
                    source,
                    receipt_time_ns,
                    bytes,
                })?;
                append_device_transport_update(&mut session, &update)?;
                if let Some(fault) = first_fatal_capture_fault(&update) {
                    terminal_error = Some(CliError::DeviceTransportFault(fault));
                }
            }
            Ok(CaptureInput::End {
                source,
                receipt_time_ns,
            }) => {
                let update = transport.handle(&DeviceTransportEvent::Disconnected {
                    source,
                    receipt_time_ns,
                })?;
                append_device_transport_update(&mut session, &update)?;
                connected.remove(&source);
            }
            Ok(CaptureInput::Failed {
                source,
                receipt_time_ns,
                error,
            }) => {
                let update = transport.handle(&DeviceTransportEvent::Disconnected {
                    source,
                    receipt_time_ns,
                })?;
                append_device_transport_update(&mut session, &update)?;
                connected.remove(&source);
                terminal_error = Some(CliError::Io(error));
            }
            Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) if signal.load(Ordering::SeqCst) != 0 => break,
            Err(RecvTimeoutError::Timeout) => {
                let host_time_ns = elapsed_ns(capture_start);
                let update = transport.handle(&DeviceTransportEvent::Tick { host_time_ns })?;
                append_device_transport_update(&mut session, &update)?;
                if let Some(fault) = first_fatal_capture_fault(&update) {
                    terminal_error = Some(CliError::DeviceTransportFault(fault));
                }
            }
        }
    }

    for source in declared_hands {
        if connected.contains(&source) {
            let update = transport.handle(&DeviceTransportEvent::Disconnected {
                source,
                receipt_time_ns: elapsed_ns(capture_start),
            })?;
            append_device_transport_update(&mut session, &update)?;
        }
    }

    let (writer, summary) = session.finish()?;
    sync_file_and_parent_directory(writer.get_ref(), Path::new(session_path))?;
    if let Some(error) = terminal_error {
        return Err(error);
    }
    writeln!(output, "evidence=recorded")?;
    writeln!(output, "controller_identity=locked")?;
    writeln!(output, "template={template_path}")?;
    writeln!(output, "manifest={locked_manifest_path}")?;
    for (hand, path) in sources {
        writeln!(output, "{}_device={path}", hand_name(*hand))?;
    }
    writeln!(
        output,
        "manifest_sha256={}",
        format_sha256(&manifest_sha256)
    )?;
    writeln!(output, "path={session_path}")?;
    writeln!(output, "total_records={}", summary.total_records)?;
    writeln!(output, "invalid_records={}", summary.invalid_records)?;
    writeln!(output, "left_received={}", summary.left.received)?;
    writeln!(output, "right_received={}", summary.right.received)?;
    writeln!(
        output,
        "malformed_candidates={}",
        summary.malformed_candidates
    )?;
    writeln!(
        output,
        "oversized_candidates={}",
        summary.oversized_candidates
    )?;
    writeln!(
        output,
        "unterminated_candidates={}",
        summary.unterminated_candidates
    )?;
    Ok(())
}

#[derive(Debug)]
enum Gate0RecorderFailure {
    StopBeforeReady(Vec<u8>),
    GuardClosedBeforeReady,
    InvalidStopBytes(Vec<u8>),
    DuplicateStopBytes(Vec<u8>),
    GuardClosedBeforeReceipt,
    Signal(ShutdownSignal),
    SourceDisconnected(Hand),
    SourceSilent(Hand),
    RequestTimeout { actual_ns: u64 },
    ReceiptTimeout { actual_ns: u64 },
}

impl fmt::Display for Gate0RecorderFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StopBeforeReady(bytes) => write!(
                formatter,
                "Gate 0A stop pipe supplied bytes before readiness: {bytes:02x?}"
            ),
            Self::GuardClosedBeforeReady => {
                formatter.write_str("Gate 0A analyzer guard closed before readiness")
            }
            Self::InvalidStopBytes(bytes) => write!(
                formatter,
                "Gate 0A expected exactly one local 0x01 stop byte; received {bytes:02x?}"
            ),
            Self::DuplicateStopBytes(bytes) => write!(
                formatter,
                "Gate 0A stop pipe supplied additional bytes after the request: {bytes:02x?}"
            ),
            Self::GuardClosedBeforeReceipt => {
                formatter.write_str("Gate 0A analyzer guard closed before a matching receipt")
            }
            Self::Signal(signal) => {
                write!(formatter, "Gate 0A recorder was aborted by {signal:?}")
            }
            Self::SourceDisconnected(hand) => {
                write!(formatter, "Gate 0A {hand:?} controller disconnected")
            }
            Self::SourceSilent(hand) => {
                write!(
                    formatter,
                    "Gate 0A {hand:?} controller became silent before stop"
                )
            }
            Self::RequestTimeout { actual_ns } => write!(
                formatter,
                "Gate 0A request was not written and drained within {GATE0A_HOST_RESPONSE_TIMEOUT_NS} ns (elapsed {actual_ns} ns)"
            ),
            Self::ReceiptTimeout { actual_ns } => write!(
                formatter,
                "Gate 0A receipt was not observed within {GATE0A_HOST_RESPONSE_TIMEOUT_NS} ns (elapsed {actual_ns} ns)"
            ),
        }
    }
}

impl Error for Gate0RecorderFailure {}

enum CaptureInput {
    Chunk {
        source: Hand,
        receipt_time_ns: u64,
        bytes: Vec<u8>,
    },
    End {
        source: Hand,
        receipt_time_ns: u64,
    },
    Failed {
        source: Hand,
        receipt_time_ns: u64,
        error: io::Error,
    },
}

struct DeviceReaders {
    receiver: MailboxReceiver<CaptureInput>,
}

impl DeviceReaders {
    fn recv_timeout(&self, timeout: Duration) -> Result<CaptureInput, RecvTimeoutError> {
        self.receiver.recv_timeout(timeout)
    }

    /// Atomically stop admission and own every complete queued input.
    fn close_and_drain(&self) -> Vec<CaptureInput> {
        self.receiver.close_and_drain()
    }
}

fn capture_stop_flag() -> io::Result<Arc<AtomicBool>> {
    let stop = Arc::new(AtomicBool::new(false));
    flag::register(SIGINT, Arc::clone(&stop))?;
    flag::register(SIGTERM, Arc::clone(&stop))?;
    Ok(stop)
}

fn capture_signal_flag() -> io::Result<Arc<AtomicUsize>> {
    let signal = Arc::new(AtomicUsize::new(0));
    flag::register_usize(SIGINT, Arc::clone(&signal), SIGINT as usize)?;
    flag::register_usize(SIGTERM, Arc::clone(&signal), SIGTERM as usize)?;
    Ok(signal)
}

const GATE0_FOOTER_COMMITTED: usize = usize::MAX;

fn pending_shutdown_signal(signal: &AtomicUsize) -> Option<ShutdownSignal> {
    match signal.load(Ordering::SeqCst) {
        0 | GATE0_FOOTER_COMMITTED => None,
        value if value == SIGINT as usize => Some(ShutdownSignal::Interrupt),
        value if value == SIGTERM as usize => Some(ShutdownSignal::Terminate),
        _ => unreachable!("only SIGINT and SIGTERM are registered"),
    }
}

#[cfg(all(test, target_os = "linux"))]
mod gate0_footer_commit_tests {
    use super::*;
    use invisible_keyboard::session::FOOTER_LEN;

    #[test]
    fn signal_store_before_footer_claim_wins_and_prevents_finish() {
        let signal = AtomicUsize::new(SIGINT as usize);

        let rejected = Gate0FooterCommit::claim(&signal)
            .expect_err("a signal accepted before the boundary must win");

        assert!(matches!(
            rejected,
            CliError::Gate0Recorder(Gate0RecorderFailure::Signal(ShutdownSignal::Interrupt))
        ));
        assert_eq!(signal.load(Ordering::SeqCst), SIGINT as usize);
    }

    #[test]
    fn footer_claim_before_signal_store_remains_committed() {
        let signal = AtomicUsize::new(0);
        let commit = Gate0FooterCommit::claim(&signal).expect("footer boundary commits");
        assert_eq!(signal.load(Ordering::SeqCst), GATE0_FOOTER_COMMITTED);

        // This is the same store performed by signal-hook. Once the commit
        // token exists, a later signal is outside the capture boundary and
        // cannot revoke the caller's right to finish.
        signal.store(SIGTERM as usize, Ordering::SeqCst);
        let session = SessionWriter::new(
            Vec::new(),
            SessionHeader {
                nominal_period_us: 5_000,
                manifest_sha256: [0x11; 32],
                session_id: [0x22; 16],
            },
        )
        .expect("empty Gate 0 session starts");
        let (bytes, summary) = commit.finish(session).expect("committed footer writes");

        assert_eq!(summary.total_records, 0);
        assert_eq!(&bytes[bytes.len() - FOOTER_LEN..][..4], b"END3");
    }
}

fn spawn_device_readers(
    sources: impl IntoIterator<Item = (Hand, File)>,
    signal: Arc<AtomicUsize>,
    capture_start: Instant,
) -> DeviceReaders {
    const CHANNEL_DEPTH: usize = 16;
    let (sender, receiver) = bounded_mailbox(CHANNEL_DEPTH);
    for (source, input) in sources {
        let source_sender = sender.clone();
        let source_signal = Arc::clone(&signal);
        thread::spawn(move || {
            read_device(source, input, source_sender, &source_signal, capture_start);
        });
    }
    drop(sender);
    DeviceReaders { receiver }
}

fn read_device(
    source: Hand,
    mut input: File,
    sender: MailboxSender<CaptureInput>,
    signal: &AtomicUsize,
    capture_start: Instant,
) {
    let mut buffer = [0_u8; 4_096];
    let mut stop_started = None;
    loop {
        match input.read(&mut buffer) {
            Ok(0) => {
                let _ = sender.publish(CaptureInput::End {
                    source,
                    receipt_time_ns: elapsed_ns(capture_start),
                });
                return;
            }
            Ok(mut length) => {
                // A USB read may end inside a COBS record. On a signal, retain
                // its suffix through the next delimiter, but never wait on a
                // continuously progressing malformed candidate indefinitely.
                let finish = if signal.load(Ordering::SeqCst) != 0 {
                    let started = stop_started.get_or_insert_with(Instant::now);
                    if let Some(delimiter) = buffer[..length].iter().position(|byte| *byte == 0) {
                        length = delimiter + 1;
                        true
                    } else {
                        started.elapsed() >= Duration::from_millis(50)
                    }
                } else {
                    false
                };
                if !sender.publish(CaptureInput::Chunk {
                    source,
                    receipt_time_ns: elapsed_ns(capture_start),
                    bytes: buffer[..length].to_vec(),
                }) {
                    return;
                }
                if finish {
                    let _ = sender.publish(CaptureInput::End {
                        source,
                        receipt_time_ns: elapsed_ns(capture_start),
                    });
                    return;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => {
                let _ = sender.publish(CaptureInput::Failed {
                    source,
                    receipt_time_ns: elapsed_ns(capture_start),
                    error,
                });
                return;
            }
        }
    }
}

fn create_session_file(path: &str) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(path)
}

fn ensure_output_absent(path: &str) -> io::Result<()> {
    if Path::new(path).try_exists()? {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("output already exists: {path}"),
        ));
    }
    Ok(())
}

fn create_capture_session(
    locked_manifest_path: &str,
    locked_manifest: &[u8],
    session_path: &str,
    header: SessionHeader,
) -> Result<SessionWriter<io::BufWriter<File>>, CliError> {
    let mut manifest_file = create_session_file(locked_manifest_path)?;
    if let Err(error) = manifest_file.write_all(locked_manifest).and_then(|()| {
        sync_file_and_parent_directory(&manifest_file, Path::new(locked_manifest_path))
    }) {
        drop(manifest_file);
        let _ = std::fs::remove_file(locked_manifest_path);
        return Err(CliError::Io(error));
    }
    drop(manifest_file);

    let session_file = match create_session_file(session_path) {
        Ok(file) => file,
        Err(error) => {
            let _ = std::fs::remove_file(locked_manifest_path);
            return Err(CliError::Io(error));
        }
    };
    match SessionWriter::new(io::BufWriter::new(session_file), header) {
        Ok(session) => Ok(session),
        Err(error) => {
            let _ = std::fs::remove_file(session_path);
            let _ = std::fs::remove_file(locked_manifest_path);
            Err(CliError::Session(error))
        }
    }
}

fn append_device_transport_update<W: Write>(
    session: &mut SessionWriter<W>,
    update: &DeviceTransportUpdate,
) -> Result<(), SessionError> {
    for candidate in &update.candidates {
        session.append_transport_candidate(
            ControllerStream::from(candidate.source),
            &candidate.candidate,
            candidate.receipt_time_ns,
        )?;
    }
    Ok(())
}

fn first_fatal_capture_fault(update: &DeviceTransportUpdate) -> Option<DeviceTransportFault> {
    update.faults.iter().copied().find(|fault| {
        matches!(
            fault,
            DeviceTransportFault::IdentityMismatch { .. }
                | DeviceTransportFault::SourceSilent { .. }
        )
    })
}

fn hand_index(hand: Hand) -> usize {
    match hand {
        Hand::Left => 0,
        Hand::Right => 1,
    }
}

const fn hand_name(hand: Hand) -> &'static str {
    match hand {
        Hand::Left => "left",
        Hand::Right => "right",
    }
}

fn elapsed_ns(start: Instant) -> u64 {
    u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

fn new_session_id() -> [u8; 16] {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let mut identifier = timestamp.to_le_bytes();
    let process = std::process::id().to_le_bytes();
    for (target, source) in identifier[12..].iter_mut().zip(process) {
        *target ^= source;
    }
    identifier
}

fn verify_session(arguments: &[String], output: &mut impl Write) -> Result<(), CliError> {
    if arguments.len() != 5 {
        return Err(CliError::Usage(
            "verify-session requires manifest, rig evidence, axis characterization, and session paths"
                .to_owned(),
        ));
    }
    let manifest_path = &arguments[1];
    let path = &arguments[4];
    let evidence = ManifestEvidenceLoader::load(&arguments[2], &arguments[3])?;
    let manifest = evidence.load_manifest(manifest_path)?;
    let supplied_sha256 = manifest.sha256()?;
    let audit = audit_session_reader(io::BufReader::new(File::open(path)?))?;
    if audit.header().manifest_sha256 != supplied_sha256 {
        return Err(CliError::ManifestMismatch {
            expected: audit.header().manifest_sha256,
            actual: supplied_sha256,
        });
    }
    let summary = audit.summary();
    manifest.validate_no_undeclared_hands(summary.left.received, summary.right.received)?;
    writeln!(output, "evidence=replay-tested")?;
    writeln!(output, "manifest={manifest_path}")?;
    writeln!(
        output,
        "manifest_sha256={}",
        format_sha256(&supplied_sha256)
    )?;
    writeln!(output, "path={path}")?;
    writeln!(
        output,
        "nominal_period_us={}",
        audit.header().nominal_period_us
    )?;
    writeln!(output, "total_records={}", summary.total_records)?;
    writeln!(output, "invalid_records={}", summary.invalid_records)?;
    writeln!(output, "decoded_candidates={}", summary.decoded_candidates)?;
    writeln!(
        output,
        "malformed_candidates={}",
        summary.malformed_candidates
    )?;
    writeln!(
        output,
        "oversized_candidates={}",
        summary.oversized_candidates
    )?;
    writeln!(
        output,
        "unterminated_candidates={}",
        summary.unterminated_candidates
    )?;
    writeln!(output, "left_received={}", summary.left.received)?;
    writeln!(output, "right_received={}", summary.right.received)?;
    writeln!(output, "left_missing={}", summary.left.missing)?;
    writeln!(output, "right_missing={}", summary.right.missing)?;
    Ok(())
}

fn score_pilot_command(arguments: &[String], output: &mut impl Write) -> Result<(), CliError> {
    if arguments.len() != 9 {
        return Err(CliError::Usage(
            "score-pilot requires manifest, rig evidence, axis characterization, session, truth JSON, truth artifact, clock mapping, and report output paths"
                .to_owned(),
        ));
    }
    let report_path = &arguments[8];
    if arguments[1..8].contains(report_path) {
        return Err(CliError::Usage(
            "score-pilot report output must not replace an input".to_owned(),
        ));
    }
    ensure_output_absent(report_path)?;
    let evidence = ManifestEvidenceLoader::load(&arguments[2], &arguments[3])?;
    let manifest = evidence.load_manifest(&arguments[1])?;
    let session = load_calibration_session(Path::new(&arguments[4]))?;
    let truth = PilotTruth::from_json(&read_bounded_path(
        Path::new(&arguments[5]),
        MAX_PILOT_TRUTH_BYTES,
    )?)?;
    let report = reduce_pilot(
        &manifest,
        &session,
        &truth,
        File::open(&arguments[6])?,
        File::open(&arguments[7])?,
    )?;
    let report_bytes = report.canonical_json()?;
    let report_sha256 = sha256(&report_bytes);
    write_new_file(Path::new(report_path), &report_bytes)?;
    let disposition = match report.disposition {
        PilotDisposition::SupportsTwoHandBuild => "supports-two-hand-build",
        PilotDisposition::Stop => "stop",
    };
    writeln!(output, "pilot_disposition={disposition}")?;
    writeln!(output, "evidence=replay-tested")?;
    writeln!(output, "hardware_truth=not-asserted")?;
    writeln!(output, "diagnostic=non-armable")?;
    writeln!(output, "manifest={}", arguments[1])?;
    writeln!(
        output,
        "manifest_sha256={}",
        format_sha256(&manifest.sha256()?)
    )?;
    writeln!(output, "session={}", arguments[4])?;
    writeln!(
        output,
        "session_sha256={}",
        format_sha256(&session.sha256())
    )?;
    writeln!(output, "report={report_path}")?;
    writeln!(output, "report_sha256={}", format_sha256(&report_sha256))?;
    Ok(())
}

fn score_gate0a_command(arguments: &[String], output: &mut impl Write) -> Result<(), CliError> {
    if arguments.len() != 8 {
        return Err(CliError::Usage(
            "score-gate0a requires manifest, rig evidence, axis characterization, session, raw capture, capture sidecar, and report output paths"
                .to_owned(),
        ));
    }
    let manifest_path = &arguments[1];
    let session_path = &arguments[4];
    let raw_capture_path = &arguments[5];
    let capture_sidecar_path = &arguments[6];
    let report_path = &arguments[7];
    if arguments[1..7].contains(report_path) {
        return Err(CliError::Usage(
            "score-gate0a report output must not replace an input".to_owned(),
        ));
    }
    ensure_output_absent(report_path)?;

    let evidence = ManifestEvidenceLoader::load(&arguments[2], &arguments[3])?;
    let manifest = evidence.load_manifest(manifest_path)?;
    let session = load_gate0_session(session_path)?;
    let trace = reduce_gate0_capture(
        &manifest,
        &session,
        Gate0CapturePaths {
            raw_capture: Path::new(raw_capture_path),
            sidecar: Path::new(capture_sidecar_path),
        },
    )?;
    let report = score_gate0a(&manifest, &session, &trace)?;
    let report_bytes = report.canonical_json()?;
    let report_sha256 = sha256(&report_bytes);
    write_new_file(Path::new(report_path), &report_bytes)?;

    writeln!(output, "verdict={}", report.verdict())?;
    writeln!(output, "manifest={manifest_path}")?;
    writeln!(
        output,
        "manifest_sha256={}",
        report.source_manifest_sha256()
    )?;
    writeln!(output, "session={session_path}")?;
    writeln!(output, "session_sha256={}", report.source_session_sha256())?;
    writeln!(output, "raw_capture={raw_capture_path}")?;
    writeln!(
        output,
        "raw_capture_sha256={}",
        format_sha256(trace.raw_capture_sha256())
    )?;
    writeln!(output, "capture_sidecar={capture_sidecar_path}")?;
    writeln!(
        output,
        "capture_sidecar_sha256={}",
        format_sha256(trace.sidecar_sha256())
    )?;
    writeln!(output, "report={report_path}")?;
    writeln!(output, "report_sha256={}", format_sha256(&report_sha256))?;
    Ok(())
}

fn physical_study_plan(arguments: &[String], output: &mut impl Write) -> Result<(), CliError> {
    if arguments.len() != 4 {
        return Err(CliError::Usage(
            "physical-study-plan requires roster JSONL, split manifest output, and analysis plan output paths"
                .to_owned(),
        ));
    }
    let roster_path = &arguments[1];
    let manifest_path = &arguments[2];
    let plan_path = &arguments[3];
    if manifest_path == plan_path {
        return Err(CliError::Usage(
            "physical-study-plan manifest and plan output paths must differ".to_owned(),
        ));
    }
    if roster_path == manifest_path || roster_path == plan_path {
        return Err(CliError::Usage(
            "physical-study-plan outputs must not replace the roster".to_owned(),
        ));
    }
    ensure_output_absent(manifest_path)?;
    ensure_output_absent(plan_path)?;

    let roster_source = read_bounded_path(
        Path::new(roster_path),
        PHYSICAL_STUDY_EVIDENCE_JSONL_MAX_BYTES,
    )?;
    let rows = decode_physical_study_jsonl(&roster_source)?;
    let manifest = PhysicalStudySplitManifest::from_rows(&rows)?;
    let manifest_bytes = manifest.canonical_json_bytes()?;
    let manifest_sha256 = format_sha256(&sha256(&manifest_bytes));
    let first = rows
        .first()
        .expect("a split manifest cannot be built from an empty roster");
    let plan = PhysicalStudyAnalysisPlan::v1(
        manifest.gate,
        manifest.round_id.clone(),
        manifest_sha256.clone(),
        FrozenStudyPipeline::from(first),
    );
    let plan_bytes = plan.canonical_json_bytes()?;
    let plan_sha256 = format_sha256(&sha256(&plan_bytes));
    write_new_file_pair(
        Path::new(manifest_path),
        &manifest_bytes,
        Path::new(plan_path),
        &plan_bytes,
    )?;

    writeln!(output, "physical_study_round=frozen")?;
    writeln!(output, "roster={roster_path}")?;
    writeln!(output, "manifest={manifest_path}")?;
    writeln!(output, "manifest_sha256={manifest_sha256}")?;
    writeln!(output, "plan={plan_path}")?;
    writeln!(output, "plan_sha256={plan_sha256}")?;
    writeln!(output, "expected_rows={}", manifest.expected_rows.len())?;
    Ok(())
}

fn physical_study_seal(arguments: &[String], output: &mut impl Write) -> Result<(), CliError> {
    if arguments.len() != 3 {
        return Err(CliError::Usage(
            "physical-study-seal requires one row JSON input and an append-only evidence JSONL path"
                .to_owned(),
        ));
    }
    let row_path = &arguments[1];
    let evidence_path = &arguments[2];
    if row_path == evidence_path {
        return Err(CliError::Usage(
            "physical-study-seal evidence output must not replace the row input".to_owned(),
        ));
    }
    let row_source = read_bounded_path(Path::new(row_path), PHYSICAL_STUDY_EVIDENCE_ROW_MAX_BYTES)?;
    let mut row = decode_physical_study_evidence_row(&row_source)?;
    if !row.row_sha256.is_empty() {
        return Err(CliError::Usage(
            "physical-study-seal requires an empty row_sha256 field".to_owned(),
        ));
    }
    seal_physical_study_evidence_row(&mut row)?;
    let row_bytes = encode_physical_study_jsonl(std::slice::from_ref(&row))?;
    append_physical_study_row(Path::new(evidence_path), &row_bytes)?;

    writeln!(output, "physical_study_row=sealed-and-appended")?;
    writeln!(output, "row={row_path}")?;
    writeln!(output, "row_sha256={}", row.row_sha256)?;
    writeln!(output, "evidence={evidence_path}")?;
    Ok(())
}

fn append_physical_study_row(path: &Path, row_bytes: &[u8]) -> Result<(), CliError> {
    let (mut evidence, created) = match OpenOptions::new()
        .read(true)
        .append(true)
        .create_new(true)
        .open(path)
    {
        Ok(file) => (file, true),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => (
            OpenOptions::new().read(true).append(true).open(path)?,
            false,
        ),
        Err(error) => return Err(error.into()),
    };

    // The lock covers every state-dependent check and the durable append. Two
    // cooperating seal processes must never validate the same stale length.
    evidence.lock()?;
    let existing_length = usize::try_from(evidence.metadata()?.len()).map_err(|_| {
        PhysicalStudyError::Arithmetic(
            "evidence JSONL byte length does not fit this platform".to_owned(),
        )
    })?;
    if existing_length != 0 {
        evidence.seek(SeekFrom::End(-1))?;
        let mut last = [0_u8; 1];
        evidence.read_exact(&mut last)?;
        if last != [b'\n'] {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "append-only physical-study evidence {} does not end in a newline",
                    path.display()
                ),
            )
            .into());
        }
    }
    let final_length = existing_length
        .checked_add(row_bytes.len())
        .ok_or_else(|| {
            PhysicalStudyError::Arithmetic("evidence JSONL byte length overflowed usize".to_owned())
        })?;
    validate_physical_study_artifact_size(PhysicalStudyArtifact::EvidenceJsonl, final_length)?;

    // Do not truncate or roll back after this point. A short append is an
    // invalid, visible round artifact that the operator must preserve.
    evidence.write_all(row_bytes)?;
    evidence.sync_all()?;
    if created {
        sync_parent_directory(path)?;
    }
    Ok(())
}

fn physical_study_score(arguments: &[String], output: &mut impl Write) -> Result<(), CliError> {
    if arguments.len() != 5 {
        return Err(CliError::Usage(
            "physical-study-score requires evidence JSONL, split manifest, analysis plan, and report output paths"
                .to_owned(),
        ));
    }
    let evidence_path = &arguments[1];
    let manifest_path = &arguments[2];
    let plan_path = &arguments[3];
    let report_path = &arguments[4];
    if arguments[1..4].contains(report_path) {
        return Err(CliError::Usage(
            "physical-study-score report output must not replace an input".to_owned(),
        ));
    }
    ensure_output_absent(report_path)?;

    // A shared lock excludes every cooperating seal writer from the first
    // evidence byte read until the exact-byte report is durable. Later
    // appends are a new snapshot and do not invalidate this bound report.
    let evidence_path_ref = Path::new(evidence_path);
    let mut evidence_source = File::open(evidence_path_ref)?;
    evidence_source.lock_shared()?;
    let report = {
        let evidence = read_bounded_file(
            &mut evidence_source,
            evidence_path_ref,
            PHYSICAL_STUDY_EVIDENCE_JSONL_MAX_BYTES,
        )?;
        let manifest = read_bounded_path(
            Path::new(manifest_path),
            PHYSICAL_STUDY_SPLIT_MANIFEST_MAX_BYTES,
        )?;
        let plan = read_bounded_path(Path::new(plan_path), PHYSICAL_STUDY_ANALYSIS_PLAN_MAX_BYTES)?;
        analyze_physical_study_jsonl(&evidence, &manifest, &plan)?
    };
    let disposition = report.disposition;
    let report_bytes = report.canonical_json_bytes()?;
    drop(report);
    let report_sha256 = format_sha256(&sha256(&report_bytes));
    write_new_file(Path::new(report_path), &report_bytes)?;
    drop(evidence_source);

    writeln!(output, "physical_study=deterministically-scored")?;
    let disposition = match disposition {
        GateDisposition::Go => "go",
        GateDisposition::NoGo => "no-go",
        GateDisposition::Inconclusive => "inconclusive",
    };
    writeln!(output, "disposition={disposition}")?;
    writeln!(output, "evidence={evidence_path}")?;
    writeln!(output, "manifest={manifest_path}")?;
    writeln!(output, "plan={plan_path}")?;
    writeln!(output, "report={report_path}")?;
    writeln!(output, "report_sha256={report_sha256}")?;
    Ok(())
}

fn required_paths<'a>(
    arguments: &'a [String],
    command: &str,
) -> Result<(&'a str, &'a str), CliError> {
    if arguments.len() == 3 {
        return Ok((&arguments[1], &arguments[2]));
    }
    Err(CliError::Usage(format!(
        "{command} requires a manifest path and a session path"
    )))
}

fn fit_session_clocks(arguments: &[String], output: &mut impl Write) -> Result<(), CliError> {
    if arguments.len() != 6 {
        return Err(CliError::Usage(
            "fit-session-clocks requires manifest, rig evidence, axis characterization, session, and output paths".to_owned(),
        ));
    }
    let manifest_path = &arguments[1];
    let session_path = &arguments[4];
    let output_path = &arguments[5];
    let evidence = ManifestEvidenceLoader::load(&arguments[2], &arguments[3])?;
    let manifest = evidence.load_manifest(manifest_path)?;
    let qualification = ClockEpochSet::audit_and_fit_reader(
        &manifest,
        io::BufReader::new(File::open(session_path)?),
    )?;
    let epochs = qualification.clock_epochs?;
    let bytes = epochs.canonical_json()?;
    write_new_file(Path::new(output_path), &bytes)?;
    writeln!(output, "clock_epochs=fitted")?;
    writeln!(output, "manifest={manifest_path}")?;
    writeln!(output, "session={session_path}")?;
    writeln!(output, "path={output_path}")?;
    writeln!(
        output,
        "clock_epoch_sha256={}",
        format_sha256(&epochs.fingerprint()?)
    )?;
    for epoch in &epochs.epochs {
        let report = epoch.fit.report();
        writeln!(
            output,
            "hand={:?} samples={} drift_ppm={:.3} residual_p95_ns={}",
            epoch.hand, report.sample_count, report.drift_ppm, report.residual_p95_ns
        )?;
    }
    Ok(())
}

fn fit_calibration_command(arguments: &[String], output: &mut impl Write) -> Result<(), CliError> {
    if arguments.len() != 10 {
        return Err(CliError::Usage(
            "fit-calibration requires manifest, rig evidence, axis characterization, clock epochs, fitting capture, surface plan, session, neutral report, and artifact output paths".to_owned(),
        ));
    }
    let evidence = ManifestEvidenceLoader::load(&arguments[2], &arguments[3])?;
    let manifest = evidence.load_manifest(&arguments[1])?;
    let clocks = ClockEpochSet::from_json(&read_bounded_path(
        Path::new(&arguments[4]),
        MAX_CLOCK_EPOCH_SET_BYTES,
    )?)?;
    let journal = CalibrationCaptureJournal::from_json(&read_bounded_path(
        Path::new(&arguments[5]),
        MAX_CALIBRATION_CAPTURE_BYTES,
    )?)?;
    validate_surface_plan_path(&journal, Path::new(&arguments[6]))?;
    let session = load_calibration_session(Path::new(&arguments[7]))?;
    let neutral_report = NeutralEvidenceReport::from_json(&read_bounded_path(
        Path::new(&arguments[8]),
        MAX_NEUTRAL_REPORT_BYTES,
    )?)?;
    let fitted = fit_calibration(CalibrationFitInput {
        manifest: &manifest,
        clocks: &clocks,
        journal: &journal,
        session: &session,
        neutral_report: &neutral_report,
    })?;
    let artifact_bytes = fitted.artifact.canonical_json()?;
    write_new_file(Path::new(&arguments[9]), &artifact_bytes)?;
    writeln!(output, "evidence=replay-tested-calibration-fit")?;
    writeln!(
        output,
        "limitation=The fitting path is deterministic; physical separability remains hardware-dependent."
    )?;
    writeln!(output, "accepted_windows={}", fitted.accepted_windows)?;
    writeln!(output, "artifact={}", arguments[9])?;
    writeln!(
        output,
        "artifact_sha256={}",
        format_sha256(&fitted.artifact.fingerprint()?)
    )?;
    Ok(())
}

fn build_calibration_evidence_command(
    arguments: &[String],
    output: &mut impl Write,
) -> Result<(), CliError> {
    if arguments.len() != 10 {
        return Err(CliError::Usage(
            "build-calibration-evidence requires manifest, rig evidence, axis characterization, clock epochs, sensor session, capture journal, surface plan, evidence output, and matching rule output paths".to_owned(),
        ));
    }
    let evidence = ManifestEvidenceLoader::load(&arguments[2], &arguments[3])?;
    let manifest = evidence.load_manifest(&arguments[1])?;
    let clocks = ClockEpochSet::from_json(&read_bounded_path(
        Path::new(&arguments[4]),
        MAX_CLOCK_EPOCH_SET_BYTES,
    )?)?;
    let session = load_calibration_session(Path::new(&arguments[5]))?;
    let journal = CalibrationCaptureJournal::from_json(&read_bounded_path(
        Path::new(&arguments[6]),
        MAX_CALIBRATION_CAPTURE_BYTES,
    )?)?;
    validate_surface_plan_path(&journal, Path::new(&arguments[7]))?;
    journal.validate_complete_surface_plan()?;
    let (evidence, report) = build_calibration_evidence(&manifest, &clocks, &journal, &session)?;
    let evidence_bytes = evidence
        .canonical_json()
        .map_err(|error| CliError::CalibrationEngine(CalibrationEngineError::Evidence(error)))?;
    write_new_file(Path::new(&arguments[8]), &evidence_bytes)?;
    let matching_rule_bytes = report.matching_rule.canonical_json()?;
    write_new_file(Path::new(&arguments[9]), &matching_rule_bytes)?;
    writeln!(output, "evidence=recorded-calibration-alignment")?;
    writeln!(
        output,
        "limitation=Window matching is implemented and replayable; its physical timing tolerance still requires hardware validation."
    )?;
    writeln!(output, "raw_keyboard_events={}", report.raw_keyboard_events)?;
    writeln!(output, "accepted_key_downs={}", report.accepted_key_downs)?;
    writeln!(
        output,
        "rejected_keyboard_events={}",
        report.rejected_keyboard_events
    )?;
    writeln!(
        output,
        "matched_observations={}",
        report.matched_observations
    )?;
    writeln!(
        output,
        "unmatched_observations={}",
        report.unmatched_observations
    )?;
    writeln!(output, "path={}", arguments[8])?;
    writeln!(output, "matching_rule={}", arguments[9])?;
    writeln!(
        output,
        "matching_rule_sha256={}",
        format_sha256(&report.matching_rule.fingerprint()?)
    )?;
    Ok(())
}

fn validate_calibration_command(
    arguments: &[String],
    output: &mut impl Write,
) -> Result<(), CliError> {
    if arguments.len() != 10 {
        return Err(CliError::Usage(
            "validate-calibration requires manifest, rig evidence, axis characterization, clock epochs, artifact, held-out capture, surface plan, session, and report output paths".to_owned(),
        ));
    }
    let evidence = ManifestEvidenceLoader::load(&arguments[2], &arguments[3])?;
    let manifest = evidence.load_manifest(&arguments[1])?;
    let clocks = ClockEpochSet::from_json(&read_bounded_path(
        Path::new(&arguments[4]),
        MAX_CLOCK_EPOCH_SET_BYTES,
    )?)?;
    let artifact = CalibrationArtifact::from_json(&read_bounded_path(
        Path::new(&arguments[5]),
        MAX_ARTIFACT_BYTES,
    )?)?;
    let journal = CalibrationCaptureJournal::from_json(&read_bounded_path(
        Path::new(&arguments[6]),
        MAX_CALIBRATION_CAPTURE_BYTES,
    )?)?;
    validate_surface_plan_path(&journal, Path::new(&arguments[7]))?;
    let session = load_calibration_session(Path::new(&arguments[8]))?;
    let validated = validate_calibration(&manifest, &clocks, &artifact, &journal, &session)?;
    let report_bytes = validated.report.canonical_json().map_err(|error| {
        CliError::CalibrationEngine(CalibrationEngineError::ValidationReport(error))
    })?;
    write_new_file(Path::new(&arguments[9]), &report_bytes)?;
    writeln!(output, "evidence=replay-tested-held-out-validation")?;
    writeln!(
        output,
        "limitation=This result qualifies only the model against finite held-out recordings. Saved files cannot prove that either wearable is still in the same position, and passive-surface feasibility still requires hardware data."
    )?;
    writeln!(output, "evaluated_windows={}", validated.evaluated_windows)?;
    writeln!(
        output,
        "macro_f1_basis_points={}",
        validated.metrics.macro_f1_basis_points
    )?;
    writeln!(
        output,
        "minimum_recall_basis_points={}",
        validated.metrics.minimum_recall_basis_points
    )?;
    writeln!(
        output,
        "heldout_validation_passed={}",
        validated.heldout_validation_passed()
    )?;
    writeln!(output, "report={}", arguments[9])?;
    Ok(())
}

fn calibrate_replay(arguments: &[String], output: &mut impl Write) -> Result<(), CliError> {
    if arguments.len() != 14 {
        return Err(CliError::Usage(
            "calibrate-replay requires manifest, rig evidence, axis characterization, fitting clocks/capture/surface-plan/session, neutral report, held-out clocks/capture/surface-plan/session, and an output directory".to_owned(),
        ));
    }
    let evidence = ManifestEvidenceLoader::load(&arguments[2], &arguments[3])?;
    let manifest = evidence.load_manifest(&arguments[1])?;
    let fitting_clocks = ClockEpochSet::from_json(&read_bounded_path(
        Path::new(&arguments[4]),
        MAX_CLOCK_EPOCH_SET_BYTES,
    )?)?;
    let fitting_journal = CalibrationCaptureJournal::from_json(&read_bounded_path(
        Path::new(&arguments[5]),
        MAX_CALIBRATION_CAPTURE_BYTES,
    )?)?;
    validate_surface_plan_path(&fitting_journal, Path::new(&arguments[6]))?;
    let fitting_session = load_calibration_session(Path::new(&arguments[7]))?;
    let neutral_report = NeutralEvidenceReport::from_json(&read_bounded_path(
        Path::new(&arguments[8]),
        MAX_NEUTRAL_REPORT_BYTES,
    )?)?;
    let validation_clocks = ClockEpochSet::from_json(&read_bounded_path(
        Path::new(&arguments[9]),
        MAX_CLOCK_EPOCH_SET_BYTES,
    )?)?;
    let validation_journal = CalibrationCaptureJournal::from_json(&read_bounded_path(
        Path::new(&arguments[10]),
        MAX_CALIBRATION_CAPTURE_BYTES,
    )?)?;
    let heldout_plan_bytes = read_bounded_path(Path::new(&arguments[11]), MAX_SURFACE_PLAN_BYTES)?;
    validation_journal
        .surface_plan()
        .ok_or(CalibrationCaptureError::MissingSurfacePlan)?
        .validate_plan_json(&heldout_plan_bytes)?;
    let validation_session = load_calibration_session(Path::new(&arguments[12]))?;
    let app = CalibrationApp::new(EvidenceMode::RecordedReplay);
    let run = run_capture_calibration(
        &app,
        CaptureCalibrationInput {
            manifest: &manifest,
            fitting_clocks: &fitting_clocks,
            fitting_journal: &fitting_journal,
            fitting_session: &fitting_session,
            neutral_report: &neutral_report,
            validation_clocks: &validation_clocks,
            validation_journal: &validation_journal,
            validation_session: &validation_session,
        },
    )?;

    let output_directory = Path::new(&arguments[13]);
    std::fs::create_dir(output_directory)?;
    let artifact_path = output_directory.join("calibration-artifact.json");
    let report_path = output_directory.join(CALIBRATION_VALIDATION_REPORT_FILE);
    let heldout_clocks_path = output_directory.join(CALIBRATION_HELDOUT_CLOCKS_FILE);
    let heldout_capture_path = output_directory.join(CALIBRATION_HELDOUT_CAPTURE_FILE);
    let heldout_surface_plan_path = output_directory.join(CALIBRATION_HELDOUT_SURFACE_PLAN_FILE);
    let heldout_evidence_path = output_directory.join(CALIBRATION_HELDOUT_EVIDENCE_FILE);
    let heldout_session_path = output_directory.join(CALIBRATION_HELDOUT_SESSION_FILE);
    let state_path = output_directory.join("setup-state.json");
    write_new_file(
        &artifact_path,
        &run.artifact.canonical_json().map_err(CliError::Artifact)?,
    )?;
    write_new_file(
        &report_path,
        &run.validation_report.canonical_json().map_err(|error| {
            CliError::CalibrationEngine(CalibrationEngineError::ValidationReport(error))
        })?,
    )?;
    write_new_file(&heldout_clocks_path, &validation_clocks.canonical_json()?)?;
    write_new_file(&heldout_capture_path, &validation_journal.canonical_json()?)?;
    write_new_file(&heldout_surface_plan_path, &heldout_plan_bytes)?;
    write_new_file(
        &heldout_evidence_path,
        &run.validation_evidence.canonical_json().map_err(|error| {
            CliError::CalibrationEngine(CalibrationEngineError::Evidence(error))
        })?,
    )?;
    write_new_file(&heldout_session_path, validation_session.bytes())?;
    write_new_file(
        &state_path,
        &serde_json::to_vec(&run.snapshot)
            .map_err(|error| CliError::Calibration(CalibrationAppError::Json(error.to_string())))?,
    )?;

    writeln!(output, "evidence=recorded-replay")?;
    writeln!(
        output,
        "limitation=The complete software flow passed the supplied immutable files; physical feasibility depends on their capture provenance."
    )?;
    writeln!(
        output,
        "keyboard_intent_separation_basis_points={}",
        run.keyboard_precheck.metrics.intent_separation_basis_points
    )?;
    writeln!(
        output,
        "keyboard_finger_agreement_basis_points={}",
        run.keyboard_precheck.metrics.finger_agreement_basis_points
    )?;
    writeln!(
        output,
        "heldout_validation_passed={}",
        run.snapshot.heldout_validation_passed
    )?;
    writeln!(output, "artifact={}", artifact_path.display())?;
    writeln!(output, "validation_report={}", report_path.display())?;
    writeln!(output, "heldout_clocks={}", heldout_clocks_path.display())?;
    writeln!(output, "heldout_capture={}", heldout_capture_path.display())?;
    writeln!(
        output,
        "heldout_surface_plan={}",
        heldout_surface_plan_path.display()
    )?;
    writeln!(
        output,
        "heldout_evidence={}",
        heldout_evidence_path.display()
    )?;
    writeln!(output, "heldout_session={}", heldout_session_path.display())?;
    writeln!(output, "setup_state={}", state_path.display())?;
    Ok(())
}

fn write_new_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(bytes)?;
    sync_file_and_parent_directory(&file, path)
}

fn hash_rig_evidence_command(
    arguments: &[String],
    output: &mut impl Write,
) -> Result<(), CliError> {
    if arguments.len() != 2 {
        return Err(CliError::Usage(
            "hash-rig-evidence requires one rig-evidence path".to_owned(),
        ));
    }
    let rig = invisible_keyboard::manifest::RigEvidenceRecord::from_json(&read_bounded_path(
        Path::new(&arguments[1]),
        MAX_RIG_EVIDENCE_BYTES,
    )?)
    .map_err(ManifestError::EvidenceArtifact)?;
    writeln!(
        output,
        "{}",
        format_sha256(&rig.sha256().map_err(ManifestError::EvidenceArtifact)?)
    )?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn load_axis_candidate(
    template: &str,
    rig: &str,
) -> Result<invisible_keyboard::manifest::AxisCommissioningCandidate, CliError> {
    Ok(
        invisible_keyboard::manifest::AxisCommissioningCandidate::from_json(
            &read_bounded_path(Path::new(template), MAX_MANIFEST_BYTES)?,
            &read_bounded_path(Path::new(rig), MAX_RIG_EVIDENCE_BYTES)?,
        )?,
    )
}

#[cfg(target_os = "linux")]
fn characterize_axes_command(
    arguments: &[String],
    output: &mut impl Write,
) -> Result<(), CliError> {
    if !(5..=6).contains(&arguments.len()) {
        return Err(CliError::Usage("characterize-axes requires template, rig evidence, new output directory, and one device per hand".to_owned()));
    }
    let candidate = load_axis_candidate(&arguments[1], &arguments[2])?;
    let directory = Path::new(&arguments[3]);
    let stop = capture_stop_flag()?;
    invisible_keyboard::axis_characterization::capture_axes(
        &candidate,
        &arguments[4..],
        directory,
        new_session_id(),
        &stop,
    )?;
    let session_path = directory.join("axis-session.iks");
    let session = FinalizedSession::audit(read_bounded_path(
        &session_path,
        MAX_SETUP_SESSION_BYTES as usize,
    )?)?;
    publish_axis_artifacts(&candidate, &session, directory, output)?;
    writeln!(output, "axis_session={}", session_path.display())?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn derive_axes_command(arguments: &[String], output: &mut impl Write) -> Result<(), CliError> {
    if arguments.len() != 5 {
        return Err(CliError::Usage(
            "derive-axes requires template, rig evidence, axis session, and new output directory"
                .to_owned(),
        ));
    }
    let candidate = load_axis_candidate(&arguments[1], &arguments[2])?;
    let session = FinalizedSession::audit(read_bounded_path(
        Path::new(&arguments[3]),
        MAX_SETUP_SESSION_BYTES as usize,
    )?)?;
    let directory = Path::new(&arguments[4]);
    // Derive before creating outputs so malformed replay input has no side effects.
    let (report, template) = axis_artifact_bytes(&candidate, &session)?;
    std::fs::create_dir(directory)?;
    write_axis_artifacts(directory, &report, &template, output)
}

#[cfg(target_os = "linux")]
fn axis_artifact_bytes(
    candidate: &invisible_keyboard::manifest::AxisCommissioningCandidate,
    session: &FinalizedSession,
) -> Result<(Vec<u8>, Vec<u8>), CliError> {
    let (utc, build) = invisible_keyboard::axis_characterization::reduction_metadata()?;
    let report =
        invisible_keyboard::axis_characterization::derive_axes(candidate, session, &utc, &build)?;
    let template = candidate.bound_template_json(&report)?;
    let report = report
        .canonical_json()
        .map_err(ManifestError::EvidenceArtifact)?;
    Ok((report, template))
}

#[cfg(target_os = "linux")]
fn publish_axis_artifacts(
    candidate: &invisible_keyboard::manifest::AxisCommissioningCandidate,
    session: &FinalizedSession,
    directory: &Path,
    output: &mut impl Write,
) -> Result<(), CliError> {
    let (report, template) = axis_artifact_bytes(candidate, session)?;
    write_axis_artifacts(directory, &report, &template, output)
}

#[cfg(target_os = "linux")]
fn write_axis_artifacts(
    directory: &Path,
    report: &[u8],
    template: &[u8],
    output: &mut impl Write,
) -> Result<(), CliError> {
    let report_path = directory.join("axis-report.json");
    let template_path = directory.join("acquisition-template.json");
    write_new_file_pair(&report_path, report, &template_path, template)?;
    writeln!(output, "axis_report={}", report_path.display())?;
    writeln!(output, "acquisition_template={}", template_path.display())?;
    writeln!(
        output,
        "authorization=none; bind evidence and observe boots before operational capture"
    )?;
    Ok(())
}

/// Publish a related artifact pair without overwriting either destination or
/// leaving one successfully created path behind after a returned failure.
fn write_new_file_pair(
    first_path: &Path,
    first_bytes: &[u8],
    second_path: &Path,
    second_bytes: &[u8],
) -> io::Result<()> {
    let mut first = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(first_path)?;
    let mut second = match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(second_path)
    {
        Ok(file) => file,
        Err(error) => {
            drop(first);
            let _ = std::fs::remove_file(first_path);
            let _ = sync_parent_directory(first_path);
            return Err(error);
        }
    };
    let publication = (|| {
        first.write_all(first_bytes)?;
        second.write_all(second_bytes)?;
        first.sync_all()?;
        second.sync_all()?;
        sync_parent_directory(first_path)?;
        if first_path.parent() != second_path.parent() {
            sync_parent_directory(second_path)?;
        }
        Ok(())
    })();
    if let Err(error) = publication {
        drop(first);
        drop(second);
        let _ = std::fs::remove_file(first_path);
        let _ = std::fs::remove_file(second_path);
        let _ = sync_parent_directory(first_path);
        if first_path.parent() != second_path.parent() {
            let _ = sync_parent_directory(second_path);
        }
        return Err(error);
    }
    Ok(())
}

/// Complete one new-file write under the host filesystem's durability API.
#[cfg(unix)]
fn sync_file_and_parent_directory(file: &File, path: &Path) -> io::Result<()> {
    file.sync_all()?;
    sync_parent_directory(path)
}

#[cfg(unix)]
fn sync_parent_directory(path: &Path) -> io::Result<()> {
    let directory = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    File::open(directory)?.sync_all()
}

#[cfg(not(unix))]
fn sync_file_and_parent_directory(file: &File, _path: &Path) -> io::Result<()> {
    file.sync_all()
}

#[cfg(not(unix))]
fn sync_parent_directory(_path: &Path) -> io::Result<()> {
    Ok(())
}

const CALIBRATION_VALIDATION_REPORT_FILE: &str = "validation-report.json";
const CALIBRATION_HELDOUT_CLOCKS_FILE: &str = "heldout-clocks.json";
const CALIBRATION_HELDOUT_CAPTURE_FILE: &str = "heldout-capture.json";
const CALIBRATION_HELDOUT_SURFACE_PLAN_FILE: &str = "heldout-surface-plan.json";
const CALIBRATION_HELDOUT_EVIDENCE_FILE: &str = "heldout-evidence.json";
const CALIBRATION_HELDOUT_SESSION_FILE: &str = "heldout-session.iks";

/// Rebuild finite model validation from the immutable held-out bundle at the
/// runtime trust boundary. A report alone is only an untrusted cache: it
/// cannot authorize replay or contribute to live authorization unless it is
/// exactly what the evaluator derives from the supplied manifest, artifact,
/// clock epochs, capture journal, and raw session.
fn load_held_out_validation(
    calibration_output_directory: &Path,
    manifest: &AcquisitionManifest,
    artifact: &CalibrationArtifact,
) -> Result<HeldOutValidation, CliError> {
    let journal = CalibrationCaptureJournal::from_json(&read_bounded_path(
        &calibration_output_directory.join(CALIBRATION_HELDOUT_CAPTURE_FILE),
        MAX_CALIBRATION_CAPTURE_BYTES,
    )?)?;
    let surface_plan = journal
        .surface_plan()
        .ok_or(CalibrationCaptureError::MissingSurfacePlan)?;
    let surface_plan_bytes = read_bounded_path(
        &calibration_output_directory.join(CALIBRATION_HELDOUT_SURFACE_PLAN_FILE),
        MAX_SURFACE_PLAN_BYTES,
    )?;
    surface_plan.validate_plan_json(&surface_plan_bytes)?;
    surface_plan.validate_locked_artifact(artifact.fingerprint()?)?;
    let clocks = ClockEpochSet::from_json(&read_bounded_path(
        &calibration_output_directory.join(CALIBRATION_HELDOUT_CLOCKS_FILE),
        MAX_CLOCK_EPOCH_SET_BYTES,
    )?)?;
    let session = load_calibration_session(
        &calibration_output_directory.join(CALIBRATION_HELDOUT_SESSION_FILE),
    )?;
    let recomputed = validate_calibration(manifest, &clocks, artifact, &journal, &session)?;
    let report_path = calibration_output_directory.join(CALIBRATION_VALIDATION_REPORT_FILE);
    let supplied_report = ValidationReport::from_json(&read_bounded_path(
        &report_path,
        MAX_VALIDATION_REPORT_BYTES,
    )?)
    .map_err(HeldOutValidationError::Report)?;
    if supplied_report != recomputed.report {
        return Err(CliError::CalibrationValidationReportMismatch {
            supplied: supplied_report
                .fingerprint()
                .map_err(HeldOutValidationError::Report)?,
            recomputed: recomputed
                .report
                .fingerprint()
                .map_err(HeldOutValidationError::Report)?,
        });
    }
    recomputed.held_out_validation().map_err(CliError::from)
}

fn replay_calibrated(arguments: &[String], output: &mut impl Write) -> Result<(), CliError> {
    if arguments.len() != 9 {
        return Err(CliError::Usage(
            "replay-calibrated requires calibration manifest, rig evidence, axis characterization, runtime manifest, clock epochs, artifact, calibration output directory, and session paths".to_owned(),
        ));
    }
    let evidence = ManifestEvidenceLoader::load(&arguments[2], &arguments[3])?;
    let calibration_manifest = evidence.load_manifest(&arguments[1])?;
    let runtime_manifest = evidence.load_manifest(&arguments[4])?;
    let clocks = ClockEpochSet::from_json(&read_bounded_path(
        Path::new(&arguments[5]),
        MAX_CLOCK_EPOCH_SET_BYTES,
    )?)?;
    let artifact = CalibrationArtifact::from_json(&read_bounded_path(
        Path::new(&arguments[6]),
        MAX_ARTIFACT_BYTES,
    )?)?;
    let held_out_validation =
        load_held_out_validation(Path::new(&arguments[7]), &calibration_manifest, &artifact)?;
    let session = load_calibration_session(Path::new(&arguments[8]))?;
    let execution = run_calibrated_replay(
        &calibration_manifest,
        &runtime_manifest,
        &clocks,
        &artifact,
        &held_out_validation,
        &session,
        invisible_keyboard::output::RecordingOutput::new(),
    )?;
    let report = &execution.report;
    writeln!(output, "evidence={}", report.evidence_label())?;
    writeln!(output, "limitation={}", report.limitation())?;
    writeln!(output, "records={}", report.total_records)?;
    writeln!(output, "decoded_frames={}", report.decoded_frames)?;
    writeln!(output, "left_frames={}", report.left_frames)?;
    writeln!(output, "right_frames={}", report.right_frames)?;
    writeln!(output, "invalid_candidates={}", report.invalid_candidates)?;
    writeln!(output, "reordered_records={}", report.reordered_records)?;
    writeln!(output, "decisions={}", report.decisions)?;
    writeln!(output, "key_decisions={}", report.key_decisions)?;
    writeln!(output, "abstentions={}", report.abstentions)?;
    writeln!(output, "runtime_faults={}", report.runtime_faults.len())?;
    writeln!(output, "output_events={}", report.output_events)?;
    writeln!(
        output,
        "processing_p50_ns={} processing_p90_ns={} processing_p95_ns={} processing_p99_ns={} processing_max_ns={}",
        report.processing_latency.p50_ns,
        report.processing_latency.p90_ns,
        report.processing_latency.p95_ns,
        report.processing_latency.p99_ns,
        report.processing_latency.max_ns
    )?;
    writeln!(
        output,
        "mapped_receipt_p50_ns={} mapped_receipt_p95_ns={} mapped_receipt_p99_ns={} mapped_receipt_max_ns={}",
        report.mapped_receipt_delta.p50_ns,
        report.mapped_receipt_delta.p95_ns,
        report.mapped_receipt_delta.p99_ns,
        report.mapped_receipt_delta.max_ns
    )?;
    writeln!(
        output,
        "final_key_state_neutral={}",
        report.final_key_state_neutral
    )?;
    for (ordinal, event) in execution.output.events().iter().enumerate() {
        writeln!(
            output,
            "event={ordinal} usage={:?} state={:?} time_us={}",
            event.usage, event.state, event.time_us
        )?;
    }
    Ok(())
}

fn run_calibrated(arguments: &[String], output: &mut impl Write) -> Result<(), CliError> {
    const WEAR_SESSION_CONFIRMATION: &str = "--confirm-wearables-unmoved";

    if !(13..=14).contains(&arguments.len()) {
        return Err(CliError::Usage(
            "run-calibrated requires calibration manifest, rig evidence, axis characterization, runtime template, artifact, calibration output directory, left device, right device, runtime manifest output, session output, live clock report output, optional output mode, and --confirm-wearables-unmoved".to_owned(),
        ));
    }
    if arguments.last().map(String::as_str) != Some(WEAR_SESSION_CONFIRMATION) {
        return Err(CliError::Usage(
            "run-calibrated requires a fresh --confirm-wearables-unmoved attestation; revalidate first if either wearable moved, loosened, or was removed".to_owned(),
        ));
    }
    if arguments[7] == arguments[8] {
        return Err(CliError::Usage(
            "run-calibrated left and right device paths must differ".to_owned(),
        ));
    }
    let runtime_manifest_path = &arguments[9];
    let session_path = &arguments[10];
    let clock_report_path = &arguments[11];
    if runtime_manifest_path == session_path
        || runtime_manifest_path == clock_report_path
        || session_path == clock_report_path
    {
        return Err(CliError::Usage(
            "run-calibrated runtime manifest, session, and live clock report output paths must be distinct".to_owned(),
        ));
    }
    let outputs = [runtime_manifest_path, session_path, clock_report_path];
    if outputs.iter().any(|path| arguments[1..=8].contains(*path)) {
        return Err(CliError::Usage(
            "run-calibrated outputs must not replace an input manifest, physical evidence, artifact, calibration output directory, or device"
                .to_owned(),
        ));
    }
    for path in outputs {
        ensure_output_absent(path)?;
    }
    let mode = if arguments.len() == 14 {
        arguments[12].as_str()
    } else {
        "recording"
    };
    validate_live_output_mode(mode)?;

    let evidence = ManifestEvidenceLoader::load(&arguments[2], &arguments[3])?;
    let calibration_manifest = evidence.load_manifest(&arguments[1])?;
    let artifact = CalibrationArtifact::from_json(&read_bounded_path(
        Path::new(&arguments[5]),
        MAX_ARTIFACT_BYTES,
    )?)?;
    let held_out_validation =
        load_held_out_validation(Path::new(&arguments[6]), &calibration_manifest, &artifact)?;
    let runtime_template = evidence.load_validation_bound_template(
        &arguments[4],
        held_out_validation.artifact_sha256(),
        held_out_validation.validation_report_sha256(),
    )?;
    let signal = capture_signal_flag()?;
    let left = open_physical_controller(&arguments[7])?;
    let right = open_physical_controller(&arguments[8])?;
    let capture_start = Instant::now();
    let input = spawn_device_readers(
        [(Hand::Left, left), (Hand::Right, right)],
        Arc::clone(&signal),
        capture_start,
    );
    let mut bootstrap = DeviceWireIdentityBootstrap::new(
        runtime_template.expected_controllers(),
        IdentityBootstrapConfig::default(),
    )?;
    let mut deferred_disconnects = Vec::new();

    while !bootstrap.is_complete() {
        if signal.load(Ordering::SeqCst) != 0 {
            return Err(CliError::IdentityBootstrap(
                IdentityBootstrapError::Incomplete,
            ));
        }
        match input.recv_timeout(Duration::from_millis(20)) {
            Ok(CaptureInput::Chunk {
                source,
                receipt_time_ns,
                bytes,
            }) => bootstrap.handle(DeviceTransportEvent::Bytes {
                source,
                receipt_time_ns,
                bytes,
            })?,
            Ok(CaptureInput::End {
                source,
                receipt_time_ns,
            }) => deferred_disconnects.push((source, receipt_time_ns)),
            Ok(CaptureInput::Failed { error, .. }) => return Err(CliError::Io(error)),
            Err(RecvTimeoutError::Disconnected) => {
                return Err(CliError::IdentityBootstrap(
                    IdentityBootstrapError::Incomplete,
                ));
            }
            Err(RecvTimeoutError::Timeout) => bootstrap.handle(DeviceTransportEvent::Tick {
                host_time_ns: elapsed_ns(capture_start),
            })?,
        }
    }

    let bootstrapped = bootstrap.finish()?;
    let runtime_manifest = runtime_template.lock(&bootstrapped.identities)?;
    let manifest_sha256 = runtime_manifest.sha256()?;
    let wear_session_confirmation =
        held_out_validation.user_attests_both_wearables_have_not_moved();
    // A caller-controlled status stream may block. Publish readiness before
    // creating the output owner or evidence session, when there is no live
    // state for a blocked writer to strand.
    writeln!(output, "wear_session={USER_ATTESTED_UNMOVED_STATUS}")?;
    writeln!(
        output,
        "User-attested wear session active. Stop typing before moving or removing either wearable."
    )?;
    output.flush()?;
    let adapter = create_live_output(mode)?;
    // Constructing the artifact-bound runtime after identity lock keeps the
    // output adapter unreachable throughout bootstrap. The runtime itself
    // remains disarmed until both online clocks pass their warmup gates.
    let mut live = LiveCalibratedRuntime::new(
        &calibration_manifest,
        &runtime_manifest,
        &artifact,
        wear_session_confirmation,
        adapter,
    )?;
    let session_id = new_session_id();
    let header = SessionHeader {
        nominal_period_us: runtime_manifest.nominal_period_us,
        manifest_sha256,
        session_id,
    };
    let mut session = create_capture_session(
        runtime_manifest_path,
        &runtime_manifest.canonical_json()?,
        session_path,
        header,
    )?;
    let mut transport =
        DeviceTransport::new(bootstrapped.identities, DeviceTransportConfig::default())?;
    let mut connected = [true, true];
    let mut live_error = None;
    let mut neutralization_failure = None;
    let mut planned_shutdown_receipt = None;
    let mut shutdown_received = None;
    let mut evidence_cutoff = LiveEvidenceCutoff::default();

    for event in bootstrapped.events {
        if planned_shutdown_receipt.is_none()
            && live_error.is_none()
            && let Some(shutdown_signal) = pending_shutdown_signal(&signal)
        {
            planned_shutdown_receipt = Some(observe_planned_live_shutdown(
                &mut live,
                &mut live_error,
                &mut neutralization_failure,
                shutdown_signal,
                capture_start,
            ));
            evidence_cutoff.close(&input);
        }
        if planned_shutdown_receipt.is_some() || live_error.is_some() {
            evidence_cutoff.defer(&input, event);
            continue;
        }
        let Some(update) = retain_live_transport_result(
            &mut live,
            &mut live_error,
            &mut neutralization_failure,
            &mut transport,
            &input,
            &mut evidence_cutoff,
            event,
        ) else {
            continue;
        };
        match apply_live_transport_update(
            &mut session,
            &mut live,
            &mut neutralization_failure,
            &input,
            &mut evidence_cutoff,
            update,
            capture_start,
        ) {
            Ok(Some(fault)) => {
                live_error = Some(CliError::DeviceTransportFault(fault));
            }
            Ok(None) => {}
            Err(error) => {
                fail_live_run(
                    &mut live,
                    &mut live_error,
                    &mut neutralization_failure,
                    error,
                );
            }
        }
        if live_error.is_some() {
            evidence_cutoff.close(&input);
        }
    }

    for (source, receipt_time_ns) in deferred_disconnects {
        if planned_shutdown_receipt.is_none()
            && live_error.is_none()
            && let Some(shutdown_signal) = pending_shutdown_signal(&signal)
        {
            planned_shutdown_receipt = Some(observe_planned_live_shutdown(
                &mut live,
                &mut live_error,
                &mut neutralization_failure,
                shutdown_signal,
                capture_start,
            ));
            evidence_cutoff.close(&input);
        }
        let event = DeviceTransportEvent::Disconnected {
            source,
            receipt_time_ns,
        };
        if planned_shutdown_receipt.is_some() || live_error.is_some() {
            evidence_cutoff.defer(&input, event);
            continue;
        }
        if let Err(error) =
            retain_source_loss_neutralization(&mut live, &mut neutralization_failure, source)
        {
            fail_live_run(
                &mut live,
                &mut live_error,
                &mut neutralization_failure,
                error,
            );
            evidence_cutoff.defer(&input, event);
            continue;
        }
        live_error = Some(CliError::DeviceTransportFault(
            DeviceTransportFault::SourceDisconnected {
                source,
                receipt_time_ns,
            },
        ));
        evidence_cutoff.defer(&input, event);
    }

    while live_error.is_none()
        && connected.iter().any(|connected| *connected)
        && planned_shutdown_receipt.is_none()
    {
        if let Some(shutdown_signal) = pending_shutdown_signal(&signal) {
            planned_shutdown_receipt = Some(observe_planned_live_shutdown(
                &mut live,
                &mut live_error,
                &mut neutralization_failure,
                shutdown_signal,
                capture_start,
            ));
            evidence_cutoff.close(&input);
            break;
        }
        let received = input.recv_timeout(Duration::from_millis(20));
        if let Some(shutdown_signal) = pending_shutdown_signal(&signal) {
            planned_shutdown_receipt = Some(observe_planned_live_shutdown(
                &mut live,
                &mut live_error,
                &mut neutralization_failure,
                shutdown_signal,
                capture_start,
            ));
            evidence_cutoff.close(&input);
            shutdown_received = received.ok();
            break;
        }
        match received {
            Ok(CaptureInput::Chunk {
                source,
                receipt_time_ns,
                bytes,
            }) => {
                let Some(update) = retain_live_transport_result(
                    &mut live,
                    &mut live_error,
                    &mut neutralization_failure,
                    &mut transport,
                    &input,
                    &mut evidence_cutoff,
                    DeviceTransportEvent::Bytes {
                        source,
                        receipt_time_ns,
                        bytes,
                    },
                ) else {
                    break;
                };
                match apply_live_transport_update(
                    &mut session,
                    &mut live,
                    &mut neutralization_failure,
                    &input,
                    &mut evidence_cutoff,
                    update,
                    capture_start,
                ) {
                    Ok(Some(fault)) => {
                        live_error = Some(CliError::DeviceTransportFault(fault));
                    }
                    Ok(None) => {}
                    Err(error) => fail_live_run(
                        &mut live,
                        &mut live_error,
                        &mut neutralization_failure,
                        error,
                    ),
                }
            }
            Ok(CaptureInput::End {
                source,
                receipt_time_ns,
            }) => {
                let event = DeviceTransportEvent::Disconnected {
                    source,
                    receipt_time_ns,
                };
                if let Err(error) = retain_source_loss_neutralization(
                    &mut live,
                    &mut neutralization_failure,
                    source,
                ) {
                    fail_live_run(
                        &mut live,
                        &mut live_error,
                        &mut neutralization_failure,
                        error,
                    );
                } else {
                    live_error = Some(CliError::DeviceTransportFault(
                        DeviceTransportFault::SourceDisconnected {
                            source,
                            receipt_time_ns,
                        },
                    ));
                }
                evidence_cutoff.defer(&input, event);
            }
            Ok(CaptureInput::Failed {
                source,
                receipt_time_ns,
                error,
            }) => {
                let event = DeviceTransportEvent::Disconnected {
                    source,
                    receipt_time_ns,
                };
                if let Err(live_source_error) = retain_source_loss_neutralization(
                    &mut live,
                    &mut neutralization_failure,
                    source,
                ) {
                    fail_live_run(
                        &mut live,
                        &mut live_error,
                        &mut neutralization_failure,
                        live_source_error,
                    );
                }
                retain_first_live_error(&mut live_error, CliError::Io(error));
                evidence_cutoff.defer(&input, event);
            }
            Err(RecvTimeoutError::Disconnected) => {
                fail_live_run(
                    &mut live,
                    &mut live_error,
                    &mut neutralization_failure,
                    CliError::Io(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "all controller reader channels closed",
                    )),
                );
                evidence_cutoff.close(&input);
                break;
            }
            Err(RecvTimeoutError::Timeout) => {
                let host_time_ns = elapsed_ns(capture_start);
                let Some(update) = retain_live_transport_result(
                    &mut live,
                    &mut live_error,
                    &mut neutralization_failure,
                    &mut transport,
                    &input,
                    &mut evidence_cutoff,
                    DeviceTransportEvent::Tick { host_time_ns },
                ) else {
                    break;
                };
                match apply_live_transport_update(
                    &mut session,
                    &mut live,
                    &mut neutralization_failure,
                    &input,
                    &mut evidence_cutoff,
                    update,
                    capture_start,
                ) {
                    Ok(Some(fault)) => {
                        live_error = Some(CliError::DeviceTransportFault(fault));
                    }
                    Ok(None) => {}
                    Err(error) => fail_live_run(
                        &mut live,
                        &mut live_error,
                        &mut neutralization_failure,
                        error,
                    ),
                }
            }
        }
        // Raw bytes are not valid-frame progress. A busy receive queue must
        // enforce the same watchdog as an idle one before another iteration.
        if live_error.is_none()
            && let Err(error) = live.tick(elapsed_ns(capture_start))
        {
            fail_live_run(
                &mut live,
                &mut live_error,
                &mut neutralization_failure,
                CliError::Live(error),
            );
        }
        if live_error.is_some() {
            evidence_cutoff.close(&input);
        }
    }

    if planned_shutdown_receipt.is_none()
        && live_error.is_none()
        && let Some(shutdown_signal) = pending_shutdown_signal(&signal)
    {
        planned_shutdown_receipt = Some(observe_planned_live_shutdown(
            &mut live,
            &mut live_error,
            &mut neutralization_failure,
            shutdown_signal,
            capture_start,
        ));
        evidence_cutoff.close(&input);
    }

    retain_terminal_input_snapshot(
        &input,
        shutdown_received,
        evidence_cutoff,
        &mut transport,
        &mut session,
        &mut connected,
        &mut live_error,
    );

    for source in [Hand::Left, Hand::Right] {
        if connected[hand_index(source)] {
            retain_shutdown_transport_event(
                DeviceTransportEvent::Disconnected {
                    source,
                    receipt_time_ns: elapsed_ns(capture_start),
                },
                &mut transport,
                &mut session,
                &mut live_error,
            );
            connected[hand_index(source)] = false;
        }
    }
    // Neutralize the output owner before any footer sync or offline fitting can
    // block. On a fault, `fail_live_run` has already cleared pending frames, so
    // this cannot emit work retained before the fault.
    let mut execution = live.finish()?;
    if execution.neutralization_failure.is_none() {
        execution.neutralization_failure = neutralization_failure;
    }
    let evidence_finalization = (|| {
        let (session_writer, _writer_summary) = finish_live_session(session, &mut live_error)?;
        sync_file_and_parent_directory(session_writer.get_ref(), Path::new(session_path))?;
        let mut session_file = session_writer
            .into_inner()
            .map_err(|error| error.into_error())?;
        session_file.seek(SeekFrom::Start(0))?;
        let qualification = ClockEpochSet::audit_and_fit_reader(
            &runtime_manifest,
            io::BufReader::new(session_file),
        )?;
        let session_summary = *qualification.session_audit.summary();
        let offline_qualification = qualification
            .clock_epochs
            .map_err(|error| error.to_string());
        let clock_report = LiveClockReport::new(
            &qualification.session_audit,
            vec![
                OperationalControllerClock {
                    hand: Hand::Left,
                    controller_id: runtime_manifest.hands[0].controller_id.clone(),
                    fit: execution.left_clock_fit,
                    diagnostics: execution.left_clock_diagnostics,
                },
                OperationalControllerClock {
                    hand: Hand::Right,
                    controller_id: runtime_manifest.hands[1].controller_id.clone(),
                    fit: execution.right_clock_fit,
                    diagnostics: execution.right_clock_diagnostics,
                },
            ],
            offline_qualification,
        )?;
        let clock_report_bytes = clock_report.canonical_json()?;
        write_new_file(Path::new(clock_report_path), &clock_report_bytes)?;
        Ok::<_, CliError>((session_summary, clock_report))
    })();
    let (session_summary, clock_report) = match evidence_finalization {
        Ok(finalized) => finalized,
        Err(finalization) => {
            attach_live_evidence_error(&mut live_error, finalization);
            return Err(live_error
                .take()
                .expect("evidence finalization installs a terminal error"));
        }
    };

    // Status streams can block indefinitely. Publish only after the evidence
    // bundle is complete, so a stalled consumer cannot interrupt durability.
    let terminal_status = (|| {
        if let Some(receipt) = &planned_shutdown_receipt {
            print_planned_shutdown_receipt(
                output,
                receipt,
                execution.neutralization_failure.as_ref(),
            )?;
        } else {
            let output_neutrality_confirmed = execution.report.output_neutrality_confirmed;
            writeln!(
                output,
                "wear_session_confirmation={}",
                execution.report.wear_session_status()
            )?;
            writeln!(
                output,
                "output_neutrality_confirmed={output_neutrality_confirmed}"
            )?;
            if let Some(failure) = &execution.neutralization_failure {
                print_neutralization_failure(output, failure)?;
            }
            if output_neutrality_confirmed {
                writeln!(
                    output,
                    "Typing stopped and output is neutral. This wear-session confirmation is no longer active. Confirm again before starting a new live process. Revalidate first if either wearable moved."
                )?;
            } else {
                writeln!(
                    output,
                    "Typing stopped and the wear-session confirmation is no longer active. Output neutrality could not be confirmed. Disconnect the virtual input device before restarting."
                )?;
            }
        }
        writeln!(output, "evidence_finalization=durable")?;
        if live_error.is_none() {
            print_live_report(
                output,
                mode,
                LiveReportPaths {
                    runtime_manifest: runtime_manifest_path,
                    session: session_path,
                    clock_report: clock_report_path,
                },
                clock_report.operational_clocks_ready(),
                clock_report.offline_evidence_qualified(),
                session_summary,
                &execution,
            )?;
        }
        output.flush()?;
        Ok::<_, CliError>(())
    })();
    if let Err(status) = terminal_status {
        return Err(CliError::LiveDurableStatusPublication {
            primary: live_error.take().map(Box::new),
            status: Box::new(status),
        });
    }
    live_error.map_or(Ok(()), Err)
}

fn validate_live_output_mode(mode: &str) -> Result<(), CliError> {
    match mode {
        "recording" => Ok(()),
        #[cfg(target_os = "linux")]
        "uinput" => Ok(()),
        #[cfg(not(target_os = "linux"))]
        "uinput" => Err(CliError::Usage(
            "uinput output is available only on Linux".to_owned(),
        )),
        unknown => Err(CliError::Usage(format!(
            "run-calibrated output mode '{unknown}' is unsupported"
        ))),
    }
}

fn create_live_output(mode: &str) -> Result<CliOutput, CliError> {
    match mode {
        "recording" => Ok(CliOutput::Recording(LiveRecordingOutput::default())),
        #[cfg(target_os = "linux")]
        "uinput" => Ok(CliOutput::Linux(Box::new(LinuxUinputOutput::new(
            "Invisible Keyboard Core-28 Live",
        )?))),
        _ => unreachable!("live output mode was validated before device admission"),
    }
}

struct PlannedShutdownReceipt {
    signal: ShutdownSignal,
    flag_observed_ns: u64,
    neutralization_latency_ns: u64,
    output_neutrality_confirmed: bool,
}

struct PlannedShutdownAttempt {
    receipt: PlannedShutdownReceipt,
    error: Option<CliError>,
}

fn begin_planned_live_shutdown(
    live: &mut LiveCalibratedRuntime<CliOutput>,
    signal: ShutdownSignal,
    capture_start: Instant,
) -> PlannedShutdownAttempt {
    // Release every held key before evidence I/O or reader coordination can
    // delay the safety response. This result is immutable after the canonical
    // output owner returns; later evidence work cannot revise the claim.
    let flag_observed_ns = elapsed_ns(capture_start);
    let neutralization_started = Instant::now();
    let error = live.shutdown(signal).map_err(CliError::from).err();
    PlannedShutdownAttempt {
        receipt: PlannedShutdownReceipt {
            signal,
            flag_observed_ns,
            neutralization_latency_ns: u64::try_from(neutralization_started.elapsed().as_nanos())
                .unwrap_or(u64::MAX),
            output_neutrality_confirmed: live.report().output_neutrality_confirmed,
        },
        error,
    }
}

fn observe_planned_live_shutdown(
    live: &mut LiveCalibratedRuntime<CliOutput>,
    live_error: &mut Option<CliError>,
    neutralization_failure: &mut Option<RuntimeOutputError<io::Error>>,
    signal: ShutdownSignal,
    capture_start: Instant,
) -> PlannedShutdownReceipt {
    let PlannedShutdownAttempt { receipt, error } =
        begin_planned_live_shutdown(live, signal, capture_start);
    if let Some(error) = error {
        fail_live_run(live, live_error, neutralization_failure, error);
    }
    receipt
}

fn print_planned_shutdown_receipt(
    output: &mut impl Write,
    receipt: &PlannedShutdownReceipt,
    neutralization_failure: Option<&RuntimeOutputError<io::Error>>,
) -> Result<(), CliError> {
    let signal = match receipt.signal {
        ShutdownSignal::Interrupt => "interrupt",
        ShutdownSignal::Terminate => "terminate",
    };
    writeln!(output, "shutdown_signal={signal}")?;
    writeln!(
        output,
        "shutdown_flag_observed_ns={}",
        receipt.flag_observed_ns
    )?;
    writeln!(
        output,
        "adapter_neutralization_latency_ns={}",
        receipt.neutralization_latency_ns
    )?;
    writeln!(
        output,
        "shutdown_neutralization={}",
        if receipt.output_neutrality_confirmed {
            "confirmed"
        } else {
            "failed"
        }
    )?;
    writeln!(
        output,
        "output_neutrality_confirmed={}",
        receipt.output_neutrality_confirmed
    )?;
    writeln!(output, "wear_session_confirmation=revoked")?;
    if let Some(failure) = neutralization_failure {
        print_neutralization_failure(output, failure)?;
    }
    if receipt.output_neutrality_confirmed {
        writeln!(
            output,
            "Typing stopped and output is neutral. This wear-session confirmation is no longer active. Confirm again before starting a new live process. Revalidate first if either wearable moved."
        )?;
    } else {
        writeln!(
            output,
            "Typing stopped and the wear-session confirmation is no longer active. Output neutrality could not be confirmed. Disconnect the virtual input device before restarting."
        )?;
    }
    Ok(())
}

/// Preserve the first operational failure after making one synchronous all-up
/// attempt. All callers continue into the shared session and report finalizer.
fn fail_live_run(
    live: &mut LiveCalibratedRuntime<CliOutput>,
    first_error: &mut Option<CliError>,
    neutralization_failure: &mut Option<RuntimeOutputError<io::Error>>,
    error: CliError,
) {
    if first_error.is_some() {
        return;
    }
    // Live operations mark their first terminal cause before returning an
    // error. Do not call a second terminal transition and replace that cause
    // with `AlreadyTerminal`; the runtime has already neutralized output.
    if live.report().terminal_reason.is_some() {
        *first_error = Some(error);
        return;
    }
    if let Err(LiveCalibratedError::Output(failure)) = live.disconnect()
        && neutralization_failure.is_none()
    {
        *neutralization_failure = Some(failure);
    }
    *first_error = Some(error);
}

fn print_neutralization_failure(
    output: &mut impl Write,
    failure: &RuntimeOutputError<io::Error>,
) -> Result<(), CliError> {
    writeln!(output, "neutralization_failure_event={:?}", failure.event)?;
    writeln!(output, "neutralization_failure_cause={}", failure.cause)?;
    match &failure.recovery_error {
        Some(error) => writeln!(output, "neutralization_failure_recovery_error={error}")?,
        None => writeln!(output, "neutralization_failure_recovery_error=none")?,
    }
    Ok(())
}

/// Revoke output before retaining a rejected event as terminal evidence.
fn retain_live_transport_result(
    live: &mut LiveCalibratedRuntime<CliOutput>,
    first_error: &mut Option<CliError>,
    neutralization_failure: &mut Option<RuntimeOutputError<io::Error>>,
    transport: &mut DeviceTransport,
    input: &DeviceReaders,
    evidence_cutoff: &mut LiveEvidenceCutoff,
    event: DeviceTransportEvent,
) -> Option<LiveTransportUpdate<io::Error>> {
    match live.admit_transport_event(transport, &event, || evidence_cutoff.close(input)) {
        Ok(mut update) => {
            if let Some(error) = update.take_ingress_error() {
                fail_live_run(
                    live,
                    first_error,
                    neutralization_failure,
                    CliError::Live(error),
                );
                evidence_cutoff.released_update = Some(update.into_evidence());
                None
            } else {
                Some(update)
            }
        }
        Err(error) => {
            fail_live_run(
                live,
                first_error,
                neutralization_failure,
                CliError::DeviceTransport(error),
            );
            evidence_cutoff.defer(input, event);
            None
        }
    }
}

#[derive(Default)]
struct LiveEvidenceCutoff {
    admitted: Option<Vec<CaptureInput>>,
    owned_events: Vec<DeviceTransportEvent>,
    released_update: Option<DeviceTransportUpdate>,
}

impl LiveEvidenceCutoff {
    fn close(&mut self, input: &DeviceReaders) {
        if self.admitted.is_none() {
            self.admitted = Some(input.close_and_drain());
        }
    }

    fn defer(&mut self, input: &DeviceReaders, event: DeviceTransportEvent) {
        self.close(input);
        self.owned_events.push(event);
    }
}

fn retain_terminal_input_snapshot<W: Write>(
    input: &DeviceReaders,
    first_input: Option<CaptureInput>,
    mut evidence_cutoff: LiveEvidenceCutoff,
    transport: &mut DeviceTransport,
    session: &mut SessionWriter<W>,
    connected: &mut [bool; 2],
    first_error: &mut Option<CliError>,
) {
    // Admission closes at the first terminal cause, before any evidence I/O.
    // Bootstrap/rejected inputs already owned by the consumer precede the
    // saved mailbox backlog in every source's byte stream.
    evidence_cutoff.close(input);
    // Ingress faults have already committed the chunk to the transport.
    // Retain released candidates once without decoding that chunk again.
    if let Some(update) = evidence_cutoff.released_update
        && let Err(error) = append_device_transport_update(session, &update)
    {
        attach_live_evidence_error(first_error, CliError::Session(error));
    }
    for event in evidence_cutoff.owned_events {
        if let DeviceTransportEvent::Disconnected { source, .. } = &event {
            connected[hand_index(*source)] = false;
        }
        retain_shutdown_transport_event(event, transport, session, first_error);
    }
    if let Some(first_input) = first_input {
        retain_shutdown_input(first_input, transport, session, connected, first_error);
    }

    // Queue residency is the canonical cutoff. Closing atomically owns every
    // admitted input without waiting for a reader blocked in `read`. A
    // complete read that loses this lock race is post-cutoff.
    for next in evidence_cutoff.admitted.expect("terminal admission closed") {
        retain_shutdown_input(next, transport, session, connected, first_error);
    }
}

fn retain_shutdown_input<W: Write>(
    input: CaptureInput,
    transport: &mut DeviceTransport,
    session: &mut SessionWriter<W>,
    connected: &mut [bool; 2],
    first_error: &mut Option<CliError>,
) {
    match input {
        CaptureInput::Chunk {
            source,
            receipt_time_ns,
            bytes,
        } => {
            retain_shutdown_transport_event(
                DeviceTransportEvent::Bytes {
                    source,
                    receipt_time_ns,
                    bytes,
                },
                transport,
                session,
                first_error,
            );
        }
        CaptureInput::End {
            source,
            receipt_time_ns,
        } => {
            retain_shutdown_transport_event(
                DeviceTransportEvent::Disconnected {
                    source,
                    receipt_time_ns,
                },
                transport,
                session,
                first_error,
            );
            connected[hand_index(source)] = false;
        }
        CaptureInput::Failed {
            source,
            receipt_time_ns,
            error,
        } => {
            retain_first_live_error(first_error, CliError::Io(error));
            retain_shutdown_transport_event(
                DeviceTransportEvent::Disconnected {
                    source,
                    receipt_time_ns,
                },
                transport,
                session,
                first_error,
            );
            connected[hand_index(source)] = false;
        }
    }
}

fn finish_live_session<W: Write>(
    session: SessionWriter<W>,
    first_error: &mut Option<CliError>,
) -> Result<(W, invisible_keyboard::session::SessionSummary), CliError> {
    // A recoverable sink error can leave a valid container with missing input.
    // Never certify that incomplete prefix merely because later writes work.
    if matches!(
        first_error,
        Some(CliError::Session(_) | CliError::LiveEvidenceFinalization { .. })
    ) {
        return Err(first_error
            .take()
            .expect("retention failure remains visible"));
    }
    Ok(session.finish()?)
}

fn retain_shutdown_transport_event<W: Write>(
    event: DeviceTransportEvent,
    transport: &mut DeviceTransport,
    session: &mut SessionWriter<W>,
    first_error: &mut Option<CliError>,
) {
    let update = match transport.retain_evidence(event) {
        Ok(update) => update,
        Err(error) => {
            attach_live_evidence_error(first_error, CliError::DeviceTransport(error));
            return;
        }
    };
    if let Err(error) = append_device_transport_update(session, &update) {
        attach_live_evidence_error(first_error, CliError::Session(error));
    }
}

fn retain_first_live_error(first_error: &mut Option<CliError>, error: CliError) {
    if first_error.is_none() {
        *first_error = Some(error);
    }
}

fn attach_live_evidence_error(first_error: &mut Option<CliError>, evidence: CliError) {
    *first_error = Some(match first_error.take() {
        Some(primary) => CliError::LiveEvidenceFinalization {
            primary: Box::new(primary),
            finalization: Box::new(evidence),
        },
        None => evidence,
    });
}

fn apply_live_transport_update(
    session: &mut SessionWriter<io::BufWriter<File>>,
    live: &mut LiveCalibratedRuntime<CliOutput>,
    neutralization_failure: &mut Option<RuntimeOutputError<io::Error>>,
    input: &DeviceReaders,
    evidence_cutoff: &mut LiveEvidenceCutoff,
    update: LiveTransportUpdate<io::Error>,
    capture_start: Instant,
) -> Result<Option<DeviceTransportFault>, CliError> {
    let fatal_fault = first_fatal_capture_fault(update.evidence());
    if let Some(fault) = fatal_fault {
        let source = match fault {
            DeviceTransportFault::IdentityMismatch { source, .. }
            | DeviceTransportFault::SourceSilent { source, .. }
            | DeviceTransportFault::SourceDisconnected { source, .. } => source,
        };
        // Neutralize the affected source before session I/O can delay the
        // safety response. The raw update is still retained after recovery.
        retain_source_loss_neutralization(live, neutralization_failure, source)?;
        evidence_cutoff.close(input);
    }
    if let Err(error) = append_device_transport_update(session, update.evidence()) {
        return Err(match fatal_fault {
            Some(fault) => CliError::LiveEvidenceFinalization {
                primary: Box::new(CliError::DeviceTransportFault(fault)),
                finalization: Box::new(CliError::Session(error)),
            },
            None => CliError::Session(error),
        });
    }
    if let Some(fault) = fatal_fault {
        return Ok(Some(fault));
    }
    // Sample after evidence I/O: neither dequeue delay nor recorder latency
    // may let historical receipt-time candidate processing release stale keys.
    live.release_transport_update(update, elapsed_ns(capture_start))?;
    Ok(None)
}

#[cfg(test)]
mod live_evidence_tests {
    use super::*;
    use invisible_keyboard::{
        protocol::{FrameIdentity, decode_frame},
        session::SessionRecordKind,
    };

    fn frame(source: Hand, sequence: u32) -> SensorFrame {
        SensorFrame {
            hand: source,
            controller_id_sha256: [if source == Hand::Left { 1 } else { 2 }; 32],
            boot_id: [3; 16],
            flags: 0,
            valid_sensor_mask: ALL_SENSORS_VALID,
            sequence,
            device_time_us: u64::from(sequence) * 5_000,
            sensors: [SensorReading::default(); SENSOR_COUNT],
        }
    }

    fn bytes(source: Hand, receipt_time_ns: u64, bytes: Vec<u8>) -> DeviceTransportEvent {
        DeviceTransportEvent::Bytes {
            source,
            receipt_time_ns,
            bytes,
        }
    }

    fn terminal_backlog(case: &str) {
        let mut transport = DeviceTransport::new(
            [Hand::Left, Hand::Right].map(|source| {
                let frame = frame(source, 1);
                FrameIdentity {
                    hand: source,
                    controller_id_sha256: frame.controller_id_sha256,
                    boot_id: frame.boot_id,
                }
            }),
            DeviceTransportConfig {
                merge_lateness_ns: 1_000,
                source_silence_timeout_ns: 10_000,
                max_pending_candidates: 2,
            },
        )
        .expect("transport topology");
        let mut session = SessionWriter::new(
            Vec::new(),
            SessionHeader {
                nominal_period_us: 5_000,
                manifest_sha256: [4; 32],
                session_id: [5; 16],
            },
        )
        .expect("session header");
        let second = encode_transport_frame(&frame(Hand::Left, 2)).expect("second frame");
        let split = second.len() / 2;
        let mut initial = encode_transport_frame(&frame(Hand::Left, 1)).expect("first frame");
        initial.extend_from_slice(&second[..split]);
        let update = transport
            .handle(&bytes(Hand::Left, 100, initial))
            .expect("initial left");
        append_device_transport_update(&mut session, &update).expect("retain initial left");
        let update = transport
            .handle(&bytes(
                Hand::Right,
                100,
                encode_transport_frame(&frame(Hand::Right, 1)).expect("right"),
            ))
            .expect("initial right");
        append_device_transport_update(&mut session, &update).expect("retain initial right");
        let trigger_time = match case {
            "late" => {
                let update = transport
                    .handle(&DeviceTransportEvent::Tick {
                        host_time_ns: 1_200,
                    })
                    .expect("commit watermark");
                append_device_transport_update(&mut session, &update).expect("retain release");
                150
            }
            "regression" => 90,
            "capacity" => 101,
            _ => unreachable!(),
        };
        let mut trigger_bytes = second[split..].to_vec();
        trigger_bytes.extend(encode_transport_frame(&frame(Hand::Left, 3)).expect("third"));
        let trigger = bytes(Hand::Left, trigger_time, trigger_bytes.clone());
        let error = transport
            .handle(&trigger)
            .expect_err("operational admission rejects trigger");
        match (case, error) {
            ("late", DeviceTransportError::MergeLatenessExceeded { .. })
            | ("regression", DeviceTransportError::ReceiptRegression { .. })
            | ("capacity", DeviceTransportError::PendingCapacityExceeded { .. }) => {}
            _ => panic!("wrong first cause: {error:?}"),
        }
        let mut first_error = Some(CliError::DeviceTransport(error));
        let (sender, receiver) = bounded_mailbox(4);
        let input = DeviceReaders { receiver };
        let mut left_backlog = encode_transport_frame(&frame(Hand::Left, 4)).expect("fourth");
        let left_suffix = vec![2, 3, 4];
        left_backlog.extend_from_slice(&left_suffix);
        assert!(sender.publish(CaptureInput::Chunk {
            source: Hand::Left,
            receipt_time_ns: 80,
            bytes: left_backlog,
        }));
        let mut right_backlog =
            encode_transport_frame(&frame(Hand::Right, 2)).expect("right second");
        let right_suffix = vec![5, 6];
        right_backlog.extend_from_slice(&right_suffix);
        assert!(sender.publish(CaptureInput::Chunk {
            source: Hand::Right,
            receipt_time_ns: 70,
            bytes: right_backlog,
        }));
        let mut evidence_cutoff = LiveEvidenceCutoff::default();
        evidence_cutoff.defer(&input, trigger);
        let mut connected = [true, true];
        retain_terminal_input_snapshot(
            &input,
            None,
            evidence_cutoff,
            &mut transport,
            &mut session,
            &mut connected,
            &mut first_error,
        );
        assert!(
            !sender.publish(CaptureInput::Chunk {
                source: Hand::Left,
                receipt_time_ns: 200,
                bytes: vec![9, 0],
            }),
            "post-cutoff input cannot become evidence"
        );
        for source in [Hand::Left, Hand::Right] {
            retain_shutdown_input(
                CaptureInput::End {
                    source,
                    receipt_time_ns: 99_999,
                },
                &mut transport,
                &mut session,
                &mut connected,
                &mut first_error,
            );
        }
        assert!(
            matches!(first_error, Some(CliError::DeviceTransport(actual)) if actual == error),
            "evidence-only admission must not replace or wrap the primary failure"
        );
        let (session_bytes, summary) = session.finish().expect("footer");
        assert_eq!(summary.total_records, 8);
        let finalized = FinalizedSession::audit(session_bytes).expect("complete evidence audit");
        let mut reader = finalized.reader();
        let mut left = Vec::new();
        let mut right = Vec::new();
        while let Some(record) = reader.next_record().expect("complete retained evidence") {
            let records = if record.source == ControllerStream::Left {
                &mut left
            } else {
                &mut right
            };
            records.push((record.kind, record.host_time_ns, record.payload));
        }
        for (records, source, sequences, times, suffix, suffix_time) in [
            (
                left,
                Hand::Left,
                vec![1, 2, 3, 4],
                vec![100, trigger_time, trigger_time, 80],
                left_suffix,
                80,
            ),
            (
                right,
                Hand::Right,
                vec![1, 2],
                vec![100, 70],
                right_suffix,
                70,
            ),
        ] {
            assert_eq!(records.len(), sequences.len() + 1);
            for ((kind, time, payload), (sequence, expected_time)) in
                records.iter().zip(sequences.into_iter().zip(times))
            {
                assert_eq!(*kind, SessionRecordKind::Decoded);
                assert_eq!(*time, expected_time);
                assert_eq!(
                    decode_frame(payload).expect("retained frame"),
                    frame(source, sequence)
                );
            }
            assert_eq!(
                records.last(),
                Some(&(SessionRecordKind::Unterminated, suffix_time, suffix))
            );
        }
    }

    #[test]
    fn late_publication_retains_trigger_and_mailbox_backlog() {
        terminal_backlog("late");
    }

    #[test]
    fn receipt_regression_retains_trigger_and_mailbox_backlog() {
        terminal_backlog("regression");
    }

    #[test]
    fn capacity_error_retains_trigger_and_mailbox_backlog() {
        terminal_backlog("capacity");
    }

    #[test]
    fn evidence_retention_failure_is_attached_without_replacing_primary() {
        let first_frame = frame(Hand::Left, 1);
        let mut transport = DeviceTransport::new(
            [FrameIdentity {
                hand: Hand::Left,
                controller_id_sha256: first_frame.controller_id_sha256,
                boot_id: first_frame.boot_id,
            }],
            DeviceTransportConfig::default(),
        )
        .expect("one source");
        let mut session_bytes = Vec::new();
        let mut session = SessionWriter::new(
            &mut session_bytes,
            SessionHeader {
                nominal_period_us: 5_000,
                manifest_sha256: [4; 32],
                session_id: [5; 16],
            },
        )
        .expect("session");
        let cause = DeviceTransportError::ReceiptRegression {
            source: Hand::Left,
            previous: 100,
            actual: 90,
        };
        let mut error = Some(CliError::DeviceTransport(cause));
        retain_shutdown_transport_event(
            bytes(Hand::Right, 70, vec![1, 0]),
            &mut transport,
            &mut session,
            &mut error,
        );
        let failure = finish_live_session(session, &mut error)
            .expect_err("incomplete retained evidence cannot receive a footer");
        assert_eq!(session_bytes.len(), invisible_keyboard::session::HEADER_LEN);
        match failure {
            CliError::LiveEvidenceFinalization {
                primary,
                finalization,
            } => {
                assert!(matches!(*primary, CliError::DeviceTransport(actual) if actual == cause));
                assert!(matches!(
                    *finalization,
                    CliError::DeviceTransport(DeviceTransportError::UndeclaredSource(Hand::Right))
                ));
            }
            other => panic!("missing evidence-retention failure: {other}"),
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[allow(unsafe_code)]
    fn terminal_snapshot_closes_before_blocking_and_failed_evidence_io() {
        use invisible_keyboard::session::HEADER_LEN;
        use std::{
            os::{fd::AsRawFd, unix::net::UnixStream},
            thread,
        };

        let (writer, mut sink) = UnixStream::pair().expect("real evidence socket");
        let capacity: libc::c_int = 4_096;
        // SAFETY: the socket is live and `capacity` is valid option storage.
        assert_eq!(
            unsafe {
                libc::setsockopt(
                    writer.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_SNDBUF,
                    (&capacity as *const libc::c_int).cast(),
                    std::mem::size_of_val(&capacity) as libc::socklen_t,
                )
            },
            0
        );
        writer
            .set_write_timeout(Some(Duration::from_secs(5)))
            .expect("bounded sink failure");
        sink.set_read_timeout(Some(Duration::from_secs(5)))
            .expect("bounded observation");
        let session = SessionWriter::new(
            writer,
            SessionHeader {
                nominal_period_us: 5_000,
                manifest_sha256: [4; 32],
                session_id: [5; 16],
            },
        )
        .expect("session header");
        sink.read_exact(&mut [0; HEADER_LEN])
            .expect("observe header before terminal work");
        let first = frame(Hand::Left, 1);
        let transport = DeviceTransport::new(
            [FrameIdentity {
                hand: Hand::Left,
                controller_id_sha256: first.controller_id_sha256,
                boot_id: first.boot_id,
            }],
            DeviceTransportConfig::default(),
        )
        .expect("source");
        let (sender, receiver) = bounded_mailbox(2);
        let input = DeviceReaders { receiver };
        let encoded = encode_transport_frame(&first).expect("frame");
        assert!(sender.publish(CaptureInput::Chunk {
            source: Hand::Left,
            receipt_time_ns: 80,
            bytes: encoded.clone(),
        }));
        let cause = DeviceTransportError::ReceiptRegression {
            source: Hand::Left,
            previous: 100,
            actual: 90,
        };
        let mut cutoff = LiveEvidenceCutoff::default();
        cutoff.defer(&input, bytes(Hand::Left, 90, encoded.repeat(256)));
        assert!(
            !sender.publish(CaptureInput::Chunk {
                source: Hand::Left,
                receipt_time_ns: 81,
                bytes: encoded.clone(),
            }),
            "first cause closes admission before the sink is used"
        );
        let finalizer = thread::spawn(move || {
            let mut transport = transport;
            let mut session = session;
            let mut error = Some(CliError::DeviceTransport(cause));
            retain_terminal_input_snapshot(
                &input,
                None,
                cutoff,
                &mut transport,
                &mut session,
                &mut [true, false],
                &mut error,
            );
            error.expect("first failure").to_string()
        });
        // The first evidence byte proves finalization entered real sink I/O.
        // The bounded socket then blocks while the reader deliberately pauses.
        sink.read_exact(&mut [0]).expect("evidence write begins");
        assert!(
            !sender.publish(CaptureInput::Chunk {
                source: Hand::Left,
                receipt_time_ns: 82,
                bytes: encoded,
            }),
            "blocked evidence I/O cannot reopen admission"
        );
        drop(sink);
        let error = finalizer.join().expect("sink failure remains recoverable");
        assert!(error.contains(&cause.to_string()));
        assert!(
            error.contains("evidence"),
            "real sink failure must be attached: {error}"
        );
    }
}

fn retain_source_loss_neutralization(
    live: &mut LiveCalibratedRuntime<CliOutput>,
    neutralization_failure: &mut Option<RuntimeOutputError<io::Error>>,
    source: Hand,
) -> Result<(), CliError> {
    match live.source_lost(source) {
        Ok(()) => Ok(()),
        Err(LiveCalibratedError::Output(failure)) => {
            if neutralization_failure.is_none() {
                *neutralization_failure = Some(failure);
            }
            Ok(())
        }
        Err(error) => Err(CliError::Live(error)),
    }
}

#[derive(Clone, Copy)]
struct LiveReportPaths<'a> {
    runtime_manifest: &'a str,
    session: &'a str,
    clock_report: &'a str,
}

fn print_live_report(
    output: &mut impl Write,
    mode: &str,
    paths: LiveReportPaths<'_>,
    operational_clocks_ready: bool,
    offline_evidence_qualified: bool,
    session: invisible_keyboard::session::SessionSummary,
    execution: &LiveCalibratedExecution<CliOutput>,
) -> Result<(), CliError> {
    let report = &execution.report;
    writeln!(output, "evidence=recorded-live-software-path")?;
    writeln!(
        output,
        "limitation=Physical feasibility depends on the connected sensor source and held-out hardware validation."
    )?;
    writeln!(output, "output_mode={mode}")?;
    writeln!(output, "controller_identity=locked")?;
    writeln!(output, "runtime_manifest={}", paths.runtime_manifest)?;
    writeln!(output, "session={}", paths.session)?;
    writeln!(output, "live_clock_report={}", paths.clock_report)?;
    writeln!(
        output,
        "operational_clocks={}",
        if operational_clocks_ready {
            "ready"
        } else {
            "not-ready"
        }
    )?;
    writeln!(
        output,
        "offline_clock_evidence={}",
        if offline_evidence_qualified {
            "qualified"
        } else {
            "not-qualified"
        }
    )?;
    writeln!(output, "session_records={}", session.total_records)?;
    writeln!(output, "invalid_records={}", session.invalid_records)?;
    writeln!(output, "decoded_frames={}", report.decoded_frames)?;
    writeln!(output, "clock_warmup_frames={}", report.clock_warmup_frames)?;
    writeln!(
        output,
        "left_clock_quality_failures={}",
        execution.left_clock_diagnostics.candidate_quality_failures
    )?;
    writeln!(
        output,
        "right_clock_quality_failures={}",
        execution.right_clock_diagnostics.candidate_quality_failures
    )?;
    writeln!(
        output,
        "left_clock_holdover_frames={}",
        execution.left_clock_diagnostics.holdover_frames
    )?;
    writeln!(
        output,
        "right_clock_holdover_frames={}",
        execution.right_clock_diagnostics.holdover_frames
    )?;
    writeln!(output, "armed_transitions={}", report.armed_transitions)?;
    writeln!(output, "decisions={}", report.decisions)?;
    writeln!(output, "key_decisions={}", report.key_decisions)?;
    writeln!(output, "abstentions={}", report.abstentions)?;
    writeln!(output, "runtime_faults={}", report.runtime_faults.len())?;
    writeln!(output, "watchdog_timeouts={}", report.watchdog_timeouts)?;
    writeln!(
        output,
        "left_watchdog_timeouts={}",
        report.left_watchdog_timeouts
    )?;
    writeln!(
        output,
        "right_watchdog_timeouts={}",
        report.right_watchdog_timeouts
    )?;
    writeln!(output, "output_events={}", report.output_events)?;
    writeln!(
        output,
        "final_key_state_neutral={}",
        report.final_key_state_neutral
    )?;
    writeln!(
        output,
        "output_neutrality_confirmed={}",
        report.output_neutrality_confirmed
    )?;
    Ok(())
}

/// Evidence is read once per command and reused when a command admits both a
/// calibration manifest and its runtime manifest or template.
struct ManifestEvidenceLoader {
    rig_evidence: Vec<u8>,
    axis_characterization: Vec<u8>,
}

impl ManifestEvidenceLoader {
    fn load(rig_evidence_path: &str, axis_characterization_path: &str) -> Result<Self, CliError> {
        Ok(Self {
            rig_evidence: read_bounded_path(Path::new(rig_evidence_path), MAX_RIG_EVIDENCE_BYTES)?,
            axis_characterization: read_bounded_path(
                Path::new(axis_characterization_path),
                MAX_AXIS_CHARACTERIZATION_BYTES,
            )?,
        })
    }

    fn load_manifest(&self, path: &str) -> Result<AcquisitionManifest, CliError> {
        let source = read_bounded_path(Path::new(path), MAX_MANIFEST_BYTES)?;
        AcquisitionManifest::from_json_with_evidence(
            &source,
            &self.rig_evidence,
            &self.axis_characterization,
        )
        .map_err(CliError::Manifest)
    }

    fn load_template(&self, path: &str) -> Result<AcquisitionManifestTemplate, CliError> {
        let source = read_bounded_path(Path::new(path), MAX_MANIFEST_BYTES)?;
        self.parse_template(&source)
    }

    fn parse_template(&self, source: &[u8]) -> Result<AcquisitionManifestTemplate, CliError> {
        AcquisitionManifestTemplate::from_json_with_evidence(
            source,
            &self.rig_evidence,
            &self.axis_characterization,
        )
        .map_err(CliError::Manifest)
    }

    fn load_validation_bound_template(
        &self,
        path: &str,
        artifact_sha256: [u8; 32],
        validation_sha256: [u8; 32],
    ) -> Result<AcquisitionManifestTemplate, CliError> {
        let source = read_bounded_path(Path::new(path), MAX_MANIFEST_BYTES)?;
        // Validate the exact evidence-bound source first so rebinding cannot
        // normalize away malformed or physically unauthorized input.
        self.parse_template(&source)?;
        let mut pending = serde_json::from_slice::<AcquisitionManifest>(&source)
            .map_err(|error| CliError::Manifest(ManifestError::Json(error.to_string())))?;
        pending.calibration_artifact_sha256 = Some(format_sha256(&artifact_sha256));
        pending.calibration_validation_sha256 = Some(format_sha256(&validation_sha256));
        let rebound = serde_json::to_vec(&pending)
            .map_err(|error| CliError::Manifest(ManifestError::Serialization(error.to_string())))?;
        self.parse_template(&rebound)
    }
}

fn load_structural_manifest(path: &str) -> Result<AcquisitionManifest, CliError> {
    let source = read_bounded_path(Path::new(path), MAX_MANIFEST_BYTES)?;
    AcquisitionManifest::from_json(&source).map_err(CliError::Manifest)
}

fn load_calibration_session(path: &Path) -> Result<FinalizedSession, CliError> {
    let mut source = File::open(path)?;
    let declared_len = source.metadata()?.len();
    validate_calibration_session_size(declared_len)?;
    let capacity =
        usize::try_from(declared_len).expect("the calibration session cap is smaller than usize");
    let mut bytes = Vec::with_capacity(capacity);
    Read::by_ref(&mut source)
        .take(MAX_SETUP_SESSION_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)?;
    validate_calibration_session_size(u64::try_from(bytes.len()).expect("length fits u64"))?;
    FinalizedSession::audit(bytes).map_err(CliError::Session)
}

fn load_gate0_session(path: &str) -> Result<FinalizedSession, CliError> {
    let mut source = File::open(path)?;
    let declared_len = source.metadata()?.len();
    validate_gate0a_session_size(declared_len)?;
    let capacity =
        usize::try_from(declared_len).expect("the Gate 0A session cap is smaller than usize");
    let mut bytes = Vec::with_capacity(capacity);
    Read::by_ref(&mut source)
        .take(MAX_GATE0A_SESSION_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)?;
    validate_gate0a_session_size(u64::try_from(bytes.len()).expect("length fits u64"))?;
    FinalizedSession::audit(bytes).map_err(CliError::Session)
}

fn validate_surface_plan_path(
    journal: &CalibrationCaptureJournal,
    path: &Path,
) -> Result<(), CliError> {
    let binding = journal
        .surface_plan()
        .ok_or(CalibrationCaptureError::MissingSurfacePlan)?;
    binding.validate_plan_json(&read_bounded_path(path, MAX_SURFACE_PLAN_BYTES)?)?;
    Ok(())
}

fn read_bounded_path(path: &Path, maximum_bytes: usize) -> Result<Vec<u8>, CliError> {
    let mut source = File::open(path)?;
    read_bounded_file(&mut source, path, maximum_bytes)
}

fn read_bounded_file(
    source: &mut File,
    path: &Path,
    maximum_bytes: usize,
) -> Result<Vec<u8>, CliError> {
    let declared_len = source.metadata()?.len();
    let maximum_u64 = u64::try_from(maximum_bytes).unwrap_or(u64::MAX);
    if declared_len > maximum_u64 {
        return Err(CliError::InputTooLarge {
            path: path.to_path_buf(),
            actual: declared_len,
            maximum: maximum_u64,
        });
    }
    let limit = u64::try_from(maximum_bytes)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    let capacity = usize::try_from(declared_len).unwrap_or(maximum_bytes);
    let mut bytes = Vec::with_capacity(capacity);
    Read::by_ref(source).take(limit).read_to_end(&mut bytes)?;
    if bytes.len() > maximum_bytes {
        return Err(CliError::InputTooLarge {
            path: path.to_path_buf(),
            actual: u64::try_from(bytes.len()).expect("bounded read length fits u64"),
            maximum: maximum_u64,
        });
    }
    Ok(bytes)
}

fn require_no_operands(arguments: &[String], command: &str) -> Result<(), CliError> {
    if arguments.len() <= 1 {
        return Ok(());
    }
    Err(CliError::Usage(format!(
        "{command} does not accept arguments"
    )))
}

fn calibration_demo(arguments: &[String], output: &mut impl Write) -> Result<(), CliError> {
    let port = optional_port(arguments, "calibration-demo")?;
    let stop = capture_stop_flag()?;
    let app = CalibrationApp::new(EvidenceMode::SimulatedReplay);
    let server = CalibrationHttpServer::bind(app, port)?;
    let address = server.local_addr()?;
    writeln!(output, "evidence=simulated-replay")?;
    writeln!(
        output,
        "limitation=This exercises the software flow and does not establish physical feasibility."
    )?;
    writeln!(output, "url=http://127.0.0.1:{}", address.port())?;
    writeln!(output, "stop=Ctrl-C")?;
    output.flush()?;
    server.serve_until(&stop)?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn calibrate_hardware(arguments: &[String], output: &mut impl Write) -> Result<(), CliError> {
    if !(8..=9).contains(&arguments.len()) {
        return Err(CliError::Usage(
            "calibrate-hardware requires template, rig evidence, axis characterization, left device, right device, keyboard device, output directory, and optional port".to_owned(),
        ));
    }
    if arguments[4] == arguments[5] {
        return Err(CliError::Usage(
            "calibrate-hardware left and right controller paths must differ".to_owned(),
        ));
    }
    if arguments[6] == arguments[4] || arguments[6] == arguments[5] {
        return Err(CliError::Usage(
            "calibrate-hardware keyboard and controller paths must differ".to_owned(),
        ));
    }
    let port = arguments.get(8).map_or(Ok(3000), |value| {
        value.parse::<u16>().map_err(|_| {
            CliError::Usage("calibrate-hardware port must be an integer from 0 to 65535".to_owned())
        })
    })?;
    let evidence = ManifestEvidenceLoader::load(&arguments[2], &arguments[3])?;
    let template = evidence.load_template(&arguments[1])?;
    let stop = capture_stop_flag()?;
    let runner = HardwareCalibrationRunner::start(HardwareCalibrationConfig {
        template,
        left_device: Path::new(&arguments[4]).to_path_buf(),
        right_device: Path::new(&arguments[5]).to_path_buf(),
        keyboard_device: Path::new(&arguments[6]).to_path_buf(),
        output_directory: Path::new(&arguments[7]).to_path_buf(),
        port,
        stop,
    })?;
    let address = runner.local_addr()?;
    writeln!(output, "evidence=hardware-capture")?;
    writeln!(
        output,
        "limitation=Held-out validation qualifies the saved model only. The system cannot detect later wearable movement, loosening, or removal."
    )?;
    writeln!(output, "url=http://127.0.0.1:{}", address.port())?;
    writeln!(output, "output_directory={}", arguments[7])?;
    writeln!(output, "stop=Ctrl-C")?;
    output.flush()?;
    let outcome = runner.run()?;
    writeln!(output, "heldout_validation_passed=true")?;
    match outcome.result_acknowledgement {
        HardwareCalibrationResultAcknowledgement::Acknowledged {
            instruction_revision,
            acknowledged_host_time_ns,
        } => {
            writeln!(output, "result_acknowledgement=acknowledged")?;
            writeln!(
                output,
                "result_acknowledgement_revision={instruction_revision}"
            )?;
            writeln!(
                output,
                "result_acknowledgement_host_time_ns={acknowledged_host_time_ns}"
            )?;
        }
        HardwareCalibrationResultAcknowledgement::TimedOut {
            instruction_revision,
            timeout_seconds,
        } => {
            writeln!(output, "result_acknowledgement=timed-out")?;
            writeln!(
                output,
                "result_acknowledgement_revision={instruction_revision}"
            )?;
            writeln!(
                output,
                "result_acknowledgement_timeout_seconds={timeout_seconds}"
            )?;
        }
    }
    writeln!(
        output,
        "artifact_sha256={}",
        format_sha256(&outcome.artifact_sha256)
    )?;
    writeln!(
        output,
        "validation_report_sha256={}",
        format_sha256(&outcome.validation_report_sha256)
    )?;
    Ok(())
}

fn optional_port(arguments: &[String], command: &str) -> Result<u16, CliError> {
    match arguments {
        [_] => Ok(3000),
        [_, port] => port.parse().map_err(|_| {
            CliError::Usage(format!("{command} port must be an integer from 0 to 65535"))
        }),
        _ => Err(CliError::Usage(format!(
            "{command} accepts at most one port"
        ))),
    }
}

#[cfg(target_os = "linux")]
fn print_linux_keyboards(output: &mut impl Write) -> io::Result<()> {
    let keyboards = list_linux_keyboards();
    writeln!(output, "keyboards={}", keyboards.len())?;
    for keyboard in keyboards {
        writeln!(
            output,
            "path={} name={:?} physical_path={:?} vendor={:04x} product={:04x}",
            keyboard.path.display(),
            keyboard.name,
            keyboard.physical_path,
            keyboard.vendor_id,
            keyboard.product_id
        )?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn check_uinput(output: &mut impl Write) -> Result<(), CliError> {
    let mut adapter = LinuxUinputOutput::new("Invisible Keyboard Core-28")?;
    adapter.release_all()?;
    writeln!(output, "uinput=available")?;
    writeln!(output, "capabilities=Core-28")?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn probe_uinput(output: &mut impl Write) -> Result<(), CliError> {
    let report = probe_core28_uinput()?;
    writeln!(output, "uinput_probe=passed")?;
    writeln!(output, "device_node={}", report.device_node.display())?;
    writeln!(output, "observed_values={:?}", report.observed_values)?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn stress_uinput(output: &mut impl Write) -> Result<(), CliError> {
    let report = stress_core28_uinput()?;
    writeln!(output, "uinput_stress=passed")?;
    writeln!(output, "device_node={}", report.device_node.display())?;
    writeln!(output, "tap_pairs={}", report.tap_pairs)?;
    writeln!(
        output,
        "fault_release_cycles={}",
        report.fault_release_cycles
    )?;
    writeln!(output, "observed_key_events={}", report.observed_key_events)?;
    writeln!(output, "round_trip_p95_us={}", report.round_trip_p95_us)?;
    writeln!(output, "round_trip_p99_us={}", report.round_trip_p99_us)?;
    writeln!(output, "stuck_keys={}", report.stuck_keys)?;
    Ok(())
}

#[derive(Debug)]
enum CliError {
    UnknownCommand(String),
    Usage(String),
    Simulation(String),
    Session(SessionError),
    Manifest(ManifestError),
    Gate0(Gate0Error),
    Gate0Trace(Gate0TraceError),
    Pilot(PilotError),
    PhysicalStudy(PhysicalStudyError),
    Topology(ObservedTopologyError),
    Artifact(ArtifactError),
    Calibration(CalibrationAppError),
    CalibrationCapture(CalibrationCaptureError),
    CalibrationEngine(CalibrationEngineError),
    HeldOutValidation(HeldOutValidationError),
    CalibrationValidationReportMismatch {
        supplied: [u8; 32],
        recomputed: [u8; 32],
    },
    NeutralEvidence(NeutralEvidenceError),
    CaptureCalibrationRun(CaptureCalibrationRunError),
    ClockEpoch(ClockEpochError),
    ClockFit(ClockEpochFitError),
    LiveClockReport(LiveClockReportError),
    Replay(CalibratedReplayError<Infallible>),
    Live(LiveCalibratedError<io::Error>),
    LiveEvidenceFinalization {
        primary: Box<CliError>,
        finalization: Box<CliError>,
    },
    LiveDurableStatusPublication {
        primary: Option<Box<CliError>>,
        status: Box<CliError>,
    },
    DeviceTransportConfiguration(DeviceTransportConfigurationError),
    DeviceTransport(DeviceTransportError),
    IdentityBootstrap(IdentityBootstrapError),
    Gate0DeviceTransport(Gate0DeviceTransportError),
    Gate0DeviceTransportFault(Gate0DeviceTransportFault),
    Gate0Recorder(Gate0RecorderFailure),
    Gate0Terminal(Gate0aTerminalError),
    #[cfg(target_os = "linux")]
    ControllerSource(ControllerSourceError),
    #[cfg(target_os = "linux")]
    Gate0ControlPipe(Gate0ControlPipeError),
    #[cfg(target_os = "linux")]
    HardwareCalibration(HardwareCalibrationError),
    DeviceTransportFault(DeviceTransportFault),
    ManifestMismatch {
        expected: [u8; 32],
        actual: [u8; 32],
    },
    InputTooLarge {
        path: std::path::PathBuf,
        actual: u64,
        maximum: u64,
    },
    Io(io::Error),
}

impl fmt::Display for CliError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownCommand(command) => write!(formatter, "unknown command '{command}'"),
            Self::Usage(message) => formatter.write_str(message),
            Self::Simulation(message) => {
                write!(formatter, "synthetic simulation failed: {message}")
            }
            Self::Io(error) => write!(formatter, "file I/O failed: {error}"),
            Self::Session(error) => write!(formatter, "session failed: {error}"),
            Self::Manifest(error) => write!(formatter, "manifest failed: {error}"),
            Self::Gate0(error) => write!(formatter, "Gate 0A failed: {error}"),
            Self::Gate0Trace(error) => write!(formatter, "Gate 0A capture failed: {error}"),
            Self::Pilot(error) => write!(formatter, "one-hand pilot failed: {error}"),
            Self::PhysicalStudy(error) => write!(formatter, "physical study failed: {error}"),
            Self::Topology(error) => write!(formatter, "session topology failed: {error}"),
            Self::Artifact(error) => write!(formatter, "artifact failed: {error}"),
            Self::Calibration(error) => write!(formatter, "calibration failed: {error}"),
            Self::CalibrationCapture(error) => {
                write!(formatter, "calibration capture failed: {error}")
            }
            Self::CalibrationEngine(error) => {
                write!(formatter, "calibration execution failed: {error}")
            }
            Self::HeldOutValidation(error) => {
                write!(formatter, "held-out validation failed: {error}")
            }
            Self::CalibrationValidationReportMismatch {
                supplied,
                recomputed,
            } => write!(
                formatter,
                "held-out validation failed: validation report does not equal the report recomputed from held-out capture and session (supplied {}, recomputed {})",
                format_sha256(supplied),
                format_sha256(recomputed)
            ),
            Self::NeutralEvidence(error) => {
                write!(formatter, "neutral evidence failed: {error}")
            }
            Self::CaptureCalibrationRun(error) => {
                write!(formatter, "calibration replay failed: {error}")
            }
            Self::ClockEpoch(error) => write!(formatter, "clock epoch failed: {error}"),
            Self::ClockFit(error) => write!(formatter, "clock fitting failed: {error}"),
            Self::LiveClockReport(error) => write!(formatter, "live clock report failed: {error}"),
            Self::Replay(error) => write!(formatter, "calibrated replay failed: {error}"),
            Self::Live(error) => write!(formatter, "calibrated live run failed: {error}"),
            Self::LiveEvidenceFinalization {
                primary,
                finalization,
            } => write!(
                formatter,
                "{primary}; live evidence finalization also failed: {finalization}"
            ),
            Self::LiveDurableStatusPublication {
                primary: Some(primary),
                status,
            } => write!(
                formatter,
                "{primary}; the live evidence bundle is durable, but terminal status publication failed: {status}"
            ),
            Self::LiveDurableStatusPublication {
                primary: None,
                status,
            } => write!(
                formatter,
                "the live evidence bundle is durable, but terminal status publication failed: {status}"
            ),
            Self::DeviceTransportConfiguration(error) => {
                write!(formatter, "device transport configuration failed: {error}")
            }
            Self::DeviceTransport(error) => write!(formatter, "device transport failed: {error}"),
            Self::IdentityBootstrap(error) => {
                write!(formatter, "controller identity bootstrap failed: {error}")
            }
            Self::Gate0DeviceTransport(error) => {
                write!(formatter, "Gate 0A device transport failed: {error}")
            }
            Self::Gate0DeviceTransportFault(error) => {
                write!(formatter, "Gate 0A device transport fault: {error}")
            }
            Self::Gate0Recorder(error) => write!(formatter, "Gate 0A recording failed: {error}"),
            Self::Gate0Terminal(error) => {
                write!(formatter, "Gate 0A terminal audit failed: {error}")
            }
            #[cfg(target_os = "linux")]
            Self::ControllerSource(error) => {
                write!(formatter, "controller source failed: {error}")
            }
            #[cfg(target_os = "linux")]
            Self::Gate0ControlPipe(error) => {
                write!(formatter, "Gate 0A control pipe failed: {error}")
            }
            #[cfg(target_os = "linux")]
            Self::HardwareCalibration(error) => {
                write!(formatter, "hardware calibration failed: {error}")
            }
            Self::DeviceTransportFault(fault) => {
                write!(formatter, "device transport fault: {fault:?}")
            }
            Self::ManifestMismatch { expected, actual } => write!(
                formatter,
                "manifest hash does not match session header: expected {}, got {}",
                format_sha256(expected),
                format_sha256(actual)
            ),
            Self::InputTooLarge {
                path,
                actual,
                maximum,
            } => write!(
                formatter,
                "input {} is {actual} bytes; maximum is {maximum}",
                path.display()
            ),
        }
    }
}

impl Error for CliError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Session(error) => Some(error),
            Self::Manifest(error) => Some(error),
            Self::Gate0(error) => Some(error),
            Self::Gate0Trace(error) => Some(error),
            Self::Pilot(error) => Some(error),
            Self::PhysicalStudy(error) => Some(error),
            Self::Topology(error) => Some(error),
            Self::Artifact(error) => Some(error),
            Self::Calibration(error) => Some(error),
            Self::CalibrationCapture(error) => Some(error),
            Self::CalibrationEngine(error) => Some(error),
            Self::HeldOutValidation(error) => Some(error),
            Self::NeutralEvidence(error) => Some(error),
            Self::CaptureCalibrationRun(error) => Some(error),
            Self::ClockEpoch(error) => Some(error),
            Self::ClockFit(error) => Some(error),
            Self::LiveClockReport(error) => Some(error),
            Self::Replay(error) => Some(error),
            Self::Live(error) => Some(error),
            Self::LiveEvidenceFinalization { primary, .. } => Some(primary.as_ref()),
            Self::LiveDurableStatusPublication {
                primary: Some(primary),
                ..
            } => Some(primary.as_ref()),
            Self::LiveDurableStatusPublication {
                primary: None,
                status,
            } => Some(status.as_ref()),
            Self::DeviceTransportConfiguration(error) => Some(error),
            Self::DeviceTransport(error) => Some(error),
            Self::IdentityBootstrap(error) => Some(error),
            Self::Gate0DeviceTransport(error) => Some(error),
            Self::Gate0DeviceTransportFault(error) => Some(error),
            Self::Gate0Recorder(error) => Some(error),
            Self::Gate0Terminal(error) => Some(error),
            #[cfg(target_os = "linux")]
            Self::ControllerSource(error) => Some(error),
            #[cfg(target_os = "linux")]
            Self::Gate0ControlPipe(error) => Some(error),
            #[cfg(target_os = "linux")]
            Self::HardwareCalibration(error) => Some(error.as_ref()),
            Self::UnknownCommand(_)
            | Self::Usage(_)
            | Self::Simulation(_)
            | Self::CalibrationValidationReportMismatch { .. }
            | Self::DeviceTransportFault(_)
            | Self::ManifestMismatch { .. }
            | Self::InputTooLarge { .. } => None,
        }
    }
}

impl From<io::Error> for CliError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<SessionError> for CliError {
    fn from(error: SessionError) -> Self {
        Self::Session(error)
    }
}

impl From<ManifestError> for CliError {
    fn from(error: ManifestError) -> Self {
        Self::Manifest(error)
    }
}

impl From<Gate0Error> for CliError {
    fn from(error: Gate0Error) -> Self {
        Self::Gate0(error)
    }
}

impl From<Gate0TraceError> for CliError {
    fn from(error: Gate0TraceError) -> Self {
        Self::Gate0Trace(error)
    }
}

impl From<PilotError> for CliError {
    fn from(error: PilotError) -> Self {
        Self::Pilot(error)
    }
}

impl From<PhysicalStudyError> for CliError {
    fn from(error: PhysicalStudyError) -> Self {
        Self::PhysicalStudy(error)
    }
}

impl From<ObservedTopologyError> for CliError {
    fn from(error: ObservedTopologyError) -> Self {
        Self::Topology(error)
    }
}

impl From<ArtifactError> for CliError {
    fn from(error: ArtifactError) -> Self {
        Self::Artifact(error)
    }
}

impl From<CalibrationAppError> for CliError {
    fn from(error: CalibrationAppError) -> Self {
        Self::Calibration(error)
    }
}

impl From<CalibrationCaptureError> for CliError {
    fn from(error: CalibrationCaptureError) -> Self {
        Self::CalibrationCapture(error)
    }
}

impl From<CaptureCalibrationRunError> for CliError {
    fn from(error: CaptureCalibrationRunError) -> Self {
        Self::CaptureCalibrationRun(error)
    }
}

impl From<CalibrationEngineError> for CliError {
    fn from(error: CalibrationEngineError) -> Self {
        Self::CalibrationEngine(error)
    }
}

impl From<HeldOutValidationError> for CliError {
    fn from(error: HeldOutValidationError) -> Self {
        Self::HeldOutValidation(error)
    }
}

impl From<NeutralEvidenceError> for CliError {
    fn from(error: NeutralEvidenceError) -> Self {
        Self::NeutralEvidence(error)
    }
}

impl From<ClockEpochError> for CliError {
    fn from(error: ClockEpochError) -> Self {
        Self::ClockEpoch(error)
    }
}

impl From<ClockEpochFitError> for CliError {
    fn from(error: ClockEpochFitError) -> Self {
        Self::ClockFit(error)
    }
}

impl From<LiveClockReportError> for CliError {
    fn from(error: LiveClockReportError) -> Self {
        Self::LiveClockReport(error)
    }
}

impl From<CalibratedReplayError<Infallible>> for CliError {
    fn from(error: CalibratedReplayError<Infallible>) -> Self {
        Self::Replay(error)
    }
}

impl From<LiveCalibratedError<io::Error>> for CliError {
    fn from(error: LiveCalibratedError<io::Error>) -> Self {
        Self::Live(error)
    }
}

impl From<DeviceTransportConfigurationError> for CliError {
    fn from(error: DeviceTransportConfigurationError) -> Self {
        Self::DeviceTransportConfiguration(error)
    }
}

impl From<DeviceTransportError> for CliError {
    fn from(error: DeviceTransportError) -> Self {
        Self::DeviceTransport(error)
    }
}

impl From<IdentityBootstrapError> for CliError {
    fn from(error: IdentityBootstrapError) -> Self {
        Self::IdentityBootstrap(error)
    }
}

impl From<Gate0DeviceTransportError> for CliError {
    fn from(error: Gate0DeviceTransportError) -> Self {
        Self::Gate0DeviceTransport(error)
    }
}

impl From<Gate0RecorderFailure> for CliError {
    fn from(error: Gate0RecorderFailure) -> Self {
        Self::Gate0Recorder(error)
    }
}

impl From<Gate0aTerminalError> for CliError {
    fn from(error: Gate0aTerminalError) -> Self {
        Self::Gate0Terminal(error)
    }
}

#[cfg(target_os = "linux")]
impl From<ControllerSourceError> for CliError {
    fn from(error: ControllerSourceError) -> Self {
        Self::ControllerSource(error)
    }
}

#[cfg(target_os = "linux")]
impl From<Gate0ControlPipeError> for CliError {
    fn from(error: Gate0ControlPipeError) -> Self {
        Self::Gate0ControlPipe(error)
    }
}

#[cfg(target_os = "linux")]
impl From<HardwareCalibrationError> for CliError {
    fn from(error: HardwareCalibrationError) -> Self {
        Self::HardwareCalibration(error)
    }
}
