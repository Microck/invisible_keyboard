//! Synthetic hardware-mode fixture for the real-Chrome receipt contract.
//!
//! This binary advances the production calibration state machine with typed
//! runner events. It does not claim sensor or physical-surface evidence. The
//! browser contract controls it over stdin so production HTTP routes remain
//! unchanged.

use std::{
    error::Error,
    io::{self, BufRead, Write},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, RecvTimeoutError},
    },
    thread,
    time::{Duration, Instant},
};

use invisible_keyboard::{
    calibration::{
        BlockId, CalibrationDomain, CalibrationPhase, KeyboardPrecheckMetrics,
        LockedArtifactIdentity,
    },
    calibration_app::{
        CalibrationAction, CalibrationApp, CalibrationHttpServer, EvidenceMode,
        HardwareCalibrationEvent, KeyboardLayoutDecision,
    },
    layout::{PhysicalPosition, all_slots, keyboard_warm_start_eligible, position_for_label},
};
use serde_json::{Value, json};

const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

fn main() -> Result<(), Box<dyn Error>> {
    let (app, lease_revision) = hardware_app_at_surface_validate()?;
    let server = CalibrationHttpServer::bind(app.clone(), 0)?;
    let backend_url = format!("http://{}", server.local_addr()?);
    let stop = Arc::new(AtomicBool::new(false));
    let service_error = Arc::new(Mutex::new(None::<String>));
    let (lease_stop_sender, lease_stop_receiver) = mpsc::channel::<()>();

    let server_stop = Arc::clone(&stop);
    let server_error = Arc::clone(&service_error);
    let server_lease_stop = lease_stop_sender.clone();
    let server_thread = thread::spawn(move || {
        if let Err(error) = server.serve_until(&server_stop) {
            *server_error
                .lock()
                .expect("service error lock is available") = Some(error.to_string());
            server_stop.store(true, Ordering::SeqCst);
            let _ = server_lease_stop.send(());
        }
    });

    // Browser receipt behavior is the subject of this fixture. Keep the real
    // capture lease alive out of band so slow Chrome startup cannot turn this
    // synthetic contract into a lease-timing test.
    let lease_app = app.clone();
    let lease_stop = Arc::clone(&stop);
    let lease_error = Arc::clone(&service_error);
    let lease_thread = thread::spawn(move || {
        loop {
            match lease_stop_receiver.recv_timeout(Duration::from_millis(500)) {
                Ok(()) | Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => {}
            }
            if lease_stop.load(Ordering::SeqCst) {
                break;
            }
            if let Err(error) =
                lease_app.apply_action(CalibrationAction::Heartbeat, lease_revision, 0)
            {
                *lease_error.lock().expect("service error lock is available") =
                    Some(format!("fixture lease maintenance failed: {error}"));
                lease_stop.store(true, Ordering::SeqCst);
                break;
            }
        }
    });

    let mut output = io::BufWriter::new(io::stdout().lock());
    emit(
        &mut output,
        &json!({
            "ok": true,
            "schema": "invisible-keyboard-synthetic-browser-fixture-v1",
            "backend_url": backend_url,
            "evidence": "synthetic hardware-mode state transitions; no physical evidence",
        }),
    )?;

    let input = io::stdin();
    for line in input.lock().lines() {
        let line = line?;
        let command = line.trim();
        let response = match run_command(command, &app, lease_revision, &service_error) {
            Ok(response) => response,
            Err(error) => json!({"ok": false, "error": error.to_string()}),
        };
        emit(&mut output, &response)?;
        if command == "quit" {
            break;
        }
    }

    stop.store(true, Ordering::SeqCst);
    let _ = lease_stop_sender.send(());
    server_thread
        .join()
        .map_err(|_| "calibration HTTP service thread panicked")?;
    lease_thread
        .join()
        .map_err(|_| "fixture lease thread panicked")?;
    Ok(())
}

fn emit(output: &mut impl Write, value: &Value) -> Result<(), Box<dyn Error>> {
    serde_json::to_writer(&mut *output, value)?;
    output.write_all(b"\n")?;
    output.flush()?;
    Ok(())
}

fn run_command(
    command: &str,
    app: &CalibrationApp,
    lease_revision: u64,
    service_error: &Mutex<Option<String>>,
) -> Result<Value, Box<dyn Error>> {
    if let Some(error) = service_error
        .lock()
        .map_err(|_| "service error lock is poisoned")?
        .clone()
    {
        return Err(error.into());
    }

    match command {
        "status" => status(app, service_error),
        "publish-first" => publish_cue(app, lease_revision, "Tap Q now.", "Q", true),
        "publish-replacement" => publish_cue(
            app,
            lease_revision,
            "Replacement W, no receipt.",
            "W",
            false,
        ),
        "publish-second" => publish_cue(app, lease_revision, "Tap E now.", "E", true),
        "quit" => Ok(json!({"ok": true, "command": "quit"})),
        _ => {
            let Some(revision) = command.strip_prefix("await-take ") else {
                return Err(format!("unknown fixture command: {command}").into());
            };
            let revision = revision.parse::<u64>()?;
            await_receipt(app, revision)
        }
    }
}

fn status(
    app: &CalibrationApp,
    service_error: &Mutex<Option<String>>,
) -> Result<Value, Box<dyn Error>> {
    let snapshot = app.snapshot()?;
    Ok(json!({
        "ok": true,
        "phase": snapshot.phase,
        "screen": snapshot.screen,
        "capture_lease_revision": snapshot.capture_lease_revision,
        "surface_instruction_revision": snapshot.surface_instruction_revision,
        "surface_target": snapshot.surface_target,
        "paused": snapshot.paused,
        "service_error": service_error
            .lock()
            .map_err(|_| "service error lock is poisoned")?
            .clone(),
    }))
}

fn publish_cue(
    app: &CalibrationApp,
    lease_revision: u64,
    target: &str,
    label: &str,
    receipt_required: bool,
) -> Result<Value, Box<dyn Error>> {
    let position =
        position_for_label(label).ok_or_else(|| format!("fixture layout lacks {label}"))?;
    let snapshot = app.apply_hardware_event(HardwareCalibrationEvent::CaptureInstruction {
        keyboard_prompt: None,
        surface_target: Some(target.to_owned()),
        surface_position: Some(position),
        surface_instruction_receipt_required: receipt_required,
        neutral_seconds: None,
        capture_lease_revision: lease_revision,
    })?;
    Ok(json!({
        "ok": true,
        "phase": snapshot.phase,
        "screen": snapshot.screen,
        "surface_target": snapshot.surface_target,
        "surface_instruction_revision": snapshot.surface_instruction_revision,
    }))
}

fn await_receipt(app: &CalibrationApp, revision: u64) -> Result<Value, Box<dyn Error>> {
    let deadline = Instant::now() + COMMAND_TIMEOUT;
    loop {
        if let Some(receipt) = app.take_surface_instruction_ack(revision)? {
            return Ok(json!({
                "ok": true,
                "instruction_revision": receipt.instruction_revision,
                "acknowledged_host_time_ns": receipt.acknowledged_host_time_ns,
            }));
        }
        if Instant::now() >= deadline {
            return Err(format!("browser did not acknowledge instruction {revision}").into());
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn hardware_app_at_surface_validate() -> Result<(CalibrationApp, u64), Box<dyn Error>> {
    let app = CalibrationApp::new(EvidenceMode::Hardware);
    app.apply_hardware_event(HardwareCalibrationEvent::Connected)?;
    app.apply_hardware_event(HardwareCalibrationEvent::HardwareChecked)?;
    let identified =
        app.apply_hardware_event(HardwareCalibrationEvent::KeyboardDeviceIdentified {
            device: "Synthetic browser-contract keyboard".to_owned(),
        })?;
    let confirmed = app.decide_keyboard_layout(
        KeyboardLayoutDecision::ConfirmUsQwerty,
        identified
            .keyboard_layout_instruction_revision
            .ok_or("layout decision lacks an instruction revision")?,
        1,
    )?;
    let lease_revision = app
        .activate_confirmed_layout_capture(
            confirmed
                .keyboard_layout_confirmation_revision
                .ok_or("layout confirmation lacks a revision")?,
        )?
        .capture_lease_revision
        .ok_or("fixture capture lacks a lease")?;
    app.apply_hardware_event(HardwareCalibrationEvent::NeutralAccepted)?;

    accept_keyboard_evidence(&app, lease_revision)?;
    app.apply_hardware_event(HardwareCalibrationEvent::BeginKeyboardPrecheck(
        BlockId::new(20),
    ))?;
    app.apply_hardware_event(HardwareCalibrationEvent::KeyboardPrecheck {
        metrics: KeyboardPrecheckMetrics::new(9_500, 9_000)?,
        evaluated_windows: 112,
        weak_positions: Vec::new(),
    })?;
    accept_surface_evidence(&app, lease_revision)?;
    app.apply_hardware_event(HardwareCalibrationEvent::BeginArtifactFit(BlockId::new(40)))?;
    app.apply_hardware_event(HardwareCalibrationEvent::ArtifactLocked(
        LockedArtifactIdentity::new([0xa1; 32], [0xb1; 32], vec![[0xc1; 32]])?,
    ))?;

    if app.control_state()?.phase != CalibrationPhase::SurfaceValidate {
        return Err("fixture did not reach held-out surface validation".into());
    }
    Ok((app, lease_revision))
}

fn accept_keyboard_evidence(
    app: &CalibrationApp,
    lease_revision: u64,
) -> Result<(), Box<dyn Error>> {
    for slot in all_slots()
        .iter()
        .filter(|slot| keyboard_warm_start_eligible(slot.position))
    {
        for block in [BlockId::new(10), BlockId::new(11)] {
            accept_examples(
                app,
                CalibrationDomain::Keyboard,
                slot.position,
                block,
                lease_revision,
                4,
            )?;
        }
    }
    Ok(())
}

fn accept_surface_evidence(
    app: &CalibrationApp,
    lease_revision: u64,
) -> Result<(), Box<dyn Error>> {
    for slot in all_slots() {
        accept_examples(
            app,
            CalibrationDomain::PassiveSurface,
            slot.position,
            BlockId::new(30),
            lease_revision,
            3,
        )?;
    }
    Ok(())
}

fn accept_examples(
    app: &CalibrationApp,
    domain: CalibrationDomain,
    position: PhysicalPosition,
    block: BlockId,
    lease_revision: u64,
    count: usize,
) -> Result<(), Box<dyn Error>> {
    for _ in 0..count {
        app.apply_hardware_event(HardwareCalibrationEvent::ExampleAccepted {
            domain,
            position,
            block,
            capture_lease_revision: lease_revision,
        })?;
    }
    Ok(())
}
