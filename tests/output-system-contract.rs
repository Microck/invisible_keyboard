#![cfg(target_os = "linux")]

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use evdev::{
    AttributeSet, BusType, Device, EventSummary, EventType, InputEvent, InputId, KeyCode,
    uinput::VirtualDevice,
};
use invisible_keyboard::{
    labels::LinuxKeyboardDevice,
    layout::{Hand, KeyUsage},
    output::{
        KeyEvent, LinuxUinputOutput, OutputAdapter, OutputSafetySession, RequiredSourceTopology,
        ShutdownSignal, observe_core28_uinput_application, probe_core28_uinput,
        stress_core28_uinput,
    },
};
use signal_hook::{
    consts::{SIGINT, SIGTERM},
    flag,
};

const REQUIRE_UINPUT_ENV: &str = "INVISIBLE_KEYBOARD_REQUIRE_UINPUT";

#[derive(Clone, Copy, Debug)]
enum PlannedProcessSignal {
    Interrupt,
    Terminate,
}

impl PlannedProcessSignal {
    const ALL: [Self; 2] = [Self::Interrupt, Self::Terminate];

    fn number(self) -> i32 {
        match self {
            Self::Interrupt => SIGINT,
            Self::Terminate => SIGTERM,
        }
    }

    #[allow(unsafe_code)]
    fn send_to(self, child: &Child) {
        let process_id = i32::try_from(child.id()).expect("child PID fits i32");
        // SAFETY: this is the owned, unreaped child. SIGINT and SIGTERM have no
        // pointer or memory-safety preconditions.
        assert_eq!(unsafe { libc::kill(process_id, self.number()) }, 0);
    }

    fn fixture_label(self) -> &'static str {
        match self {
            Self::Interrupt => "sigint",
            Self::Terminate => "sigterm",
        }
    }

    fn display_name(self) -> &'static str {
        match self {
            Self::Interrupt => "SIGINT",
            Self::Terminate => "SIGTERM",
        }
    }

    fn shutdown_signal(self) -> ShutdownSignal {
        match self {
            Self::Interrupt => ShutdownSignal::Interrupt,
            Self::Terminate => ShutdownSignal::Terminate,
        }
    }

    fn from_number(number: usize) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|signal| signal.number() as usize == number)
    }
}

#[test]
fn native_probe_verifies_core28_identity_and_non_root_access() {
    let report = match probe_core28_uinput() {
        Ok(report) => report,
        Err(error) if uinput_capability_unavailable(&error) => {
            assert!(
                env::var_os(REQUIRE_UINPUT_ENV).is_none(),
                "native identity probe requires uinput capability: {error}"
            );
            eprintln!("SKIP native identity probe: {error}");
            return;
        }
        Err(error) => panic!("native identity probe failed: {error}"),
    };

    // probe_core28_uinput opens the registered event node and rejects any
    // identity other than BUS_VIRTUAL 0000:0000 version 0001 with the exact
    // `invisible-keyboard/core28` physical path and 28-key capability set.
    assert_eq!(report.evidence_label, "linux-kernel-event-node-readback");
    assert_eq!(report.observed_values, [1, 0]);
    assert_eq!(report.stuck_keys, 0);
    assert_ne!(
        report.access.effective_uid, 0,
        "the required lane must prove /dev/uinput access without root"
    );
    eprintln!(
        "native uinput probe: node={}, effective_uid={}, effective_gid={}, /dev/uinput={:04o} {}:{}, event={:04o} {}:{}, identity=BUS_VIRTUAL 0000:0000 version 0001 phys=invisible-keyboard/core28, observed={:?}, stuck_keys={}",
        report.device_node.display(),
        report.access.effective_uid,
        report.access.effective_gid,
        report.access.uinput_mode,
        report.access.uinput_uid,
        report.access.uinput_gid,
        report.access.event_node_mode,
        report.access.event_node_uid,
        report.access.event_node_gid,
        report.observed_values,
        report.stuck_keys,
    );
}

#[test]
fn selected_keyboard_open_queries_keys_held_before_reader_start() {
    // Label input requires a regular keyboard, including Enter. The Core-28
    // output device deliberately lacks it and is not a valid input fixture.
    let mut keys = AttributeSet::<KeyCode>::new();
    for key in [
        KeyCode::KEY_A,
        KeyCode::KEY_Z,
        KeyCode::KEY_SPACE,
        KeyCode::KEY_ENTER,
    ] {
        keys.insert(key);
    }
    let mut keyboard_device = match VirtualDevice::builder().and_then(|builder| {
        builder
            .name("Invisible Keyboard Label Test")
            .input_id(InputId::new(BusType::BUS_VIRTUAL, 0, 0, 1))
            .with_phys(c"invisible-keyboard/label-test")?
            .with_keys(&keys)?
            .build()
    }) {
        Ok(output) => output,
        Err(error) if uinput_capability_unavailable(&error) => {
            assert!(
                env::var_os(REQUIRE_UINPUT_ENV).is_none(),
                "held-state probe requires uinput capability: {error}"
            );
            eprintln!("SKIP held-state probe: {error}");
            return;
        }
        Err(error) => panic!("held-state probe failed to create its keyboard fixture: {error}"),
    };
    let device_node = keyboard_device
        .enumerate_dev_nodes_blocking()
        .expect("fixture event nodes enumerate")
        .next()
        .expect("fixture registers an event node")
        .expect("fixture event node resolves");
    // Registration precedes udev permissions. Reuse the bounded readiness
    // wait and hold a grab so the fixture cannot type into a desktop app.
    let mut observer = open_application_observer(&device_node)
        .expect("test-only label-keyboard udev rule grants fixture read access");
    observer
        .grab()
        .expect("fixture events stay inside this test");
    keyboard_device
        .emit(&[InputEvent::new(EventType::KEY.0, KeyCode::KEY_A.0, 1)])
        .expect("held-state probe submits a down transition before reader open");

    let keyboard = LinuxKeyboardDevice::open(&device_node)
        .expect("selected keyboard opens after the key is already held");
    assert!(
        keyboard
            .held_key_codes()
            .expect("selected keyboard queries current kernel state")
            .contains(&KeyCode::KEY_A.0),
        "startup state must include a key held before reader creation"
    );

    keyboard_device
        .emit(&[InputEvent::new(EventType::KEY.0, KeyCode::KEY_A.0, 0)])
        .expect("held-state probe returns the device to neutral");
}

#[test]
fn native_kernel_stress_executes_ten_thousand_pairs_and_finishes_neutral() {
    let report = match stress_core28_uinput() {
        Ok(report) => report,
        Err(error) if uinput_capability_unavailable(&error) => {
            assert!(
                env::var_os(REQUIRE_UINPUT_ENV).is_none(),
                "native kernel stress requires uinput capability: {error}"
            );
            eprintln!("SKIP native kernel stress: {error}");
            return;
        }
        Err(error) => panic!("native kernel stress failed: {error}"),
    };

    assert_eq!(report.evidence_label, "linux-kernel-event-node-readback");
    assert_eq!(report.tap_pairs, 10_000);
    assert_eq!(report.fault_release_cycles, 10);
    assert_eq!(report.observed_key_events, 20_020);
    assert_eq!(report.stuck_keys, 0);
    assert!(report.round_trip_p95_us <= report.round_trip_p99_us);
    assert!(report.round_trip_p95_us <= 100_000);
    assert!(report.round_trip_p99_us <= 200_000);
    eprintln!("native uinput stress: {report:#?}");
}

#[test]
fn native_evdev_application_observes_every_core28_pair() {
    let report = match observe_core28_uinput_application() {
        Ok(report) => report,
        Err(error) if uinput_capability_unavailable(&error) => {
            assert!(
                env::var_os(REQUIRE_UINPUT_ENV).is_none(),
                "native evdev application oracle requires uinput capability: {error}"
            );
            eprintln!("SKIP native evdev application oracle: {error}");
            return;
        }
        Err(error) => panic!("native evdev application oracle failed: {error}"),
    };

    assert_eq!(
        report.application_evidence_label,
        "native-userspace-evdev-consumer"
    );
    assert_eq!(
        report.kernel_readback_evidence_label,
        "linux-kernel-event-node-readback"
    );
    assert_ne!(
        report.application_evidence_label,
        report.kernel_readback_evidence_label
    );
    assert_eq!(report.advertised_usages, 28);
    assert_eq!(report.tap_pairs, 10_000);
    assert_eq!(report.application_observed_events, 20_000);
    assert_eq!(report.application_stuck_keys, 0);
    assert_eq!(report.kernel_readback_stuck_keys, 0);
    assert!(report.application_round_trip_p95_us <= report.application_round_trip_p99_us);
    assert!(report.application_round_trip_p95_us <= 100_000);
    assert!(report.application_round_trip_p99_us <= 200_000);
}

#[test]
fn planned_signals_are_application_observed_neutral_within_250ms() {
    const CHILD_MODE: &str = "INVISIBLE_KEYBOARD_PLANNED_SHUTDOWN_CHILD";
    const CHILD_DIRECTORY: &str = "INVISIBLE_KEYBOARD_PLANNED_SHUTDOWN_DIRECTORY";
    if env::var_os(CHILD_MODE).is_some() {
        let directory = PathBuf::from(
            env::var_os(CHILD_DIRECTORY).expect("planned-shutdown child directory is configured"),
        );
        let signal = Arc::new(AtomicUsize::new(0));
        for process_signal in PlannedProcessSignal::ALL {
            flag::register_usize(
                process_signal.number(),
                Arc::clone(&signal),
                process_signal.number() as usize,
            )
            .expect("planned-shutdown child installs its signal flag");
        }

        let mut output = LinuxUinputOutput::new("Invisible Keyboard Planned Shutdown Child")
            .expect("planned-shutdown child creates uinput device");
        let device_node = output
            .device_node()
            .expect("planned-shutdown child discovers its event node");
        let mut application = open_application_observer(&device_node)
            .expect("planned-shutdown child opens an independent evdev consumer");
        application
            .grab()
            .expect("independent evdev consumer exclusively grabs the device");
        let mut safety =
            OutputSafetySession::start(output, RequiredSourceTopology::One(Hand::Left))
                .expect("planned-shutdown child starts neutral");
        safety
            .source_connected(Hand::Left)
            .expect("planned-shutdown source connects");
        let lease = safety
            .acquire_run_ownership()
            .expect("planned-shutdown run owns output");
        safety
            .apply(lease, KeyEvent::down(KeyUsage::A, 1))
            .expect("planned-shutdown child submits a held key");
        observe_application_key_transition(
            &mut application,
            KeyCode::KEY_A,
            1,
            Duration::from_secs(1),
        )
        .expect("independent evdev consumer sees the held key");
        fs::write(directory.join("held-key-observed"), [])
            .expect("planned-shutdown child publishes readiness");

        let wait_deadline = Instant::now() + Duration::from_secs(15);
        let received_signal = loop {
            if let Some(received) = PlannedProcessSignal::from_number(signal.load(Ordering::SeqCst))
            {
                break received;
            }
            assert!(
                Instant::now() < wait_deadline,
                "planned-shutdown child did not receive SIGINT or SIGTERM"
            );
            thread::sleep(Duration::from_millis(1));
        };
        let neutralization_started = Instant::now();
        safety
            .shutdown(received_signal.shutdown_signal(), 2)
            .expect("canonical planned shutdown confirms all-up");
        observe_application_key_transition(
            &mut application,
            KeyCode::KEY_A,
            0,
            Duration::from_secs(1),
        )
        .expect("independent evdev consumer sees the release");
        let pending_receipt = directory.join("application-neutral.pending");
        fs::write(
            &pending_receipt,
            neutralization_started.elapsed().as_nanos().to_string(),
        )
        .expect("planned-shutdown child writes native observation");
        fs::rename(pending_receipt, directory.join("application-neutral"))
            .expect("planned-shutdown child atomically publishes native observation");
        drop(application);
        return;
    }

    if let Err(error) = probe_core28_uinput() {
        if uinput_capability_unavailable(&error) {
            assert!(
                env::var_os(REQUIRE_UINPUT_ENV).is_none(),
                "planned-signal observation requires uinput capability: {error}"
            );
            eprintln!("SKIP planned-signal uinput observation: {error}");
            return;
        }
        panic!("planned-signal capability probe failed: {error}");
    }

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time follows the Unix epoch")
        .as_nanos();
    let directory = env::temp_dir().join(format!(
        "invisible-keyboard-planned-shutdown-{}-{unique}",
        std::process::id()
    ));
    fs::create_dir(&directory).expect("create planned-shutdown test directory");
    let directory_guard = RemoveDirectoryOnDrop(directory);

    for process_signal in PlannedProcessSignal::ALL {
        for cycle in 0..10 {
            let context = format!("{} cycle {cycle}", process_signal.display_name());
            let cycle_directory = directory_guard
                .path()
                .join(format!("{}-cycle-{cycle}", process_signal.fixture_label()));
            fs::create_dir(&cycle_directory).expect("create planned-shutdown cycle directory");
            let mut child = KillChildOnDrop(
                Command::new(env::current_exe().expect("test executable path exists"))
                    .args([
                        "--exact",
                        "planned_signals_are_application_observed_neutral_within_250ms",
                        "--nocapture",
                    ])
                    .env(CHILD_MODE, "1")
                    .env(CHILD_DIRECTORY, &cycle_directory)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::inherit())
                    .spawn()
                    .expect("spawn planned-shutdown helper process"),
            );
            assert!(
                wait_for_path(
                    &cycle_directory.join("held-key-observed"),
                    Duration::from_secs(5),
                ),
                "{context} did not reach a held native application state"
            );

            // Time signal delivery through observed release, not executable startup.
            let signal_started = Instant::now();
            process_signal.send_to(&child.0);
            let neutral_receipt = cycle_directory.join("application-neutral");
            assert!(
                wait_for_path(&neutral_receipt, Duration::from_millis(250)),
                "{context} did not reach application-observed neutral within 250 ms"
            );
            let parent_observation = signal_started.elapsed();
            assert!(parent_observation <= Duration::from_millis(250));
            let child_observation_ns = std::str::from_utf8(
                &fs::read(&neutral_receipt).expect("read child neutralization duration"),
            )
            .expect("child neutralization duration is UTF-8")
            .parse::<u128>()
            .expect("child neutralization duration is numeric");
            assert!(child_observation_ns <= 250_000_000);
            let status = wait_for_child_exit(
                &mut child.0,
                Duration::from_secs(2),
                "planned-shutdown helper",
            );
            assert!(status.success(), "{context} helper exits successfully");
        }
    }
}

#[test]
fn sigkill_destroys_the_owned_uinput_device_without_a_250ms_claim() {
    const CHILD_MODE: &str = "INVISIBLE_KEYBOARD_SIGKILL_CHILD";
    const CHILD_DIRECTORY: &str = "INVISIBLE_KEYBOARD_SIGKILL_DIRECTORY";
    if env::var_os(CHILD_MODE).is_some() {
        let directory = PathBuf::from(
            env::var_os(CHILD_DIRECTORY).expect("SIGKILL child directory is configured"),
        );
        let mut output = LinuxUinputOutput::new("Invisible Keyboard SIGKILL Child")
            .expect("SIGKILL child creates uinput device after parent capability probe");
        let device_node = output
            .grab_event_node()
            .expect("SIGKILL child exclusively grabs its event node");
        fs::write(
            directory.join("device-node"),
            device_node.as_os_str().as_encoded_bytes(),
        )
        .expect("SIGKILL child publishes its device node");
        assert!(wait_for_path(
            &directory.join("parent-ready"),
            Duration::from_secs(5)
        ));
        output
            .emit(KeyEvent::down(KeyUsage::A, 1))
            .expect("SIGKILL child submits a down transition");
        assert_eq!(
            output
                .grabbed_pressed_key_count()
                .expect("SIGKILL child reads its grabbed kernel key state"),
            1
        );
        fs::write(directory.join("down-submitted"), [])
            .expect("SIGKILL child publishes transition receipt");
        thread::sleep(Duration::from_secs(15));
        return;
    }

    if let Err(error) = probe_core28_uinput() {
        if uinput_capability_unavailable(&error) {
            assert!(
                env::var_os(REQUIRE_UINPUT_ENV).is_none(),
                "SIGKILL destruction probe requires uinput capability: {error}"
            );
            eprintln!("SKIP SIGKILL uinput destruction probe: {error}");
            return;
        }
        panic!("SIGKILL capability probe failed: {error}");
    }

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time follows the Unix epoch")
        .as_nanos();
    let directory = env::temp_dir().join(format!(
        "invisible-keyboard-sigkill-{}-{unique}",
        std::process::id()
    ));
    fs::create_dir(&directory).expect("create exact SIGKILL test directory");
    let directory_guard = RemoveDirectoryOnDrop(directory);
    let mut child = KillChildOnDrop(
        Command::new(env::current_exe().expect("test executable path exists"))
            .args([
                "--exact",
                "sigkill_destroys_the_owned_uinput_device_without_a_250ms_claim",
                "--nocapture",
            ])
            .env(CHILD_MODE, "1")
            .env(CHILD_DIRECTORY, directory_guard.path())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn isolated SIGKILL helper process"),
    );

    let node_file = directory_guard.path().join("device-node");
    assert!(
        wait_for_path(&node_file, Duration::from_secs(5)),
        "SIGKILL child did not publish its uinput node"
    );
    let device_node = PathBuf::from(
        std::str::from_utf8(&fs::read(&node_file).expect("read child device node"))
            .expect("device node is UTF-8"),
    );
    fs::write(directory_guard.path().join("parent-ready"), [])
        .expect("release child to submit its transition");
    assert!(
        wait_for_path(
            &directory_guard.path().join("down-submitted"),
            Duration::from_secs(5),
        ),
        "SIGKILL child did not submit its down transition"
    );

    let destruction_started = Instant::now();
    child.0.kill().expect("SIGKILL the isolated child");
    let status = child.0.wait().expect("reap SIGKILL child");
    assert!(!status.success());
    assert!(wait_for_absence(&device_node, Duration::from_secs(5)));
    eprintln!(
        "SIGKILL device destruction observed after {:?}; this is not a 250 ms all-up guarantee",
        destruction_started.elapsed()
    );

    let restarted = probe_core28_uinput()
        .expect("a fresh uinput session completes from neutral after device destruction");
    assert_eq!(restarted.observed_values, [1, 0]);
    assert_eq!(restarted.stuck_keys, 0);
}

struct RemoveDirectoryOnDrop(PathBuf);

impl RemoveDirectoryOnDrop {
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for RemoveDirectoryOnDrop {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct KillChildOnDrop(Child);

impl Drop for KillChildOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn wait_for_child_exit(child: &mut Child, timeout: Duration, context: &str) -> ExitStatus {
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait().expect("child status polls") {
            return status;
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            let status = child.wait().expect("timed-out child is reaped");
            panic!("{context} did not exit within {timeout:?}; status={status}");
        }
        thread::sleep(Duration::from_millis(1));
    }
}

fn wait_for_path(path: &Path, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if path.exists() {
            return true;
        }
        thread::sleep(Duration::from_millis(5));
    }
    path.exists()
}

fn wait_for_absence(path: &Path, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if !path.exists() {
            return true;
        }
        thread::sleep(Duration::from_millis(5));
    }
    !path.exists()
}

fn open_application_observer(device_node: &Path) -> std::io::Result<Device> {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match Device::open(device_node) {
            Ok(device) => return Ok(device),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
                ) && Instant::now() < deadline =>
            {
                thread::sleep(Duration::from_millis(5));
            }
            Err(error) => return Err(error),
        }
    }
}

fn observe_application_key_transition(
    application: &mut Device,
    expected_key: KeyCode,
    expected_value: i32,
    timeout: Duration,
) -> std::io::Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        for event in application.fetch_events()? {
            if let EventSummary::Key(_, key, value) = event.destructure()
                && key == expected_key
            {
                if value == expected_value {
                    return Ok(());
                }
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "application observer expected {expected_key:?} value {expected_value}, got {value}"
                    ),
                ));
            }
        }
        if Instant::now() >= deadline {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!(
                    "application observer did not receive {expected_key:?} value {expected_value}"
                ),
            ));
        }
        thread::sleep(Duration::from_millis(1));
    }
}

fn uinput_capability_unavailable(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
    )
}
