//! Fail-neutral key state and the single output-adapter seam.

use std::{
    collections::BTreeSet,
    convert::Infallible,
    sync::atomic::{AtomicU64, Ordering},
};

use crate::layout::{Hand, KeyUsage};

static NEXT_OUTPUT_SESSION_ID: AtomicU64 = AtomicU64::new(1);

/// A key transition understood by operating-system and HID adapters.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EventState {
    Down,
    Up,
}

/// One timestamped exact key transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct KeyEvent {
    pub usage: KeyUsage,
    pub state: EventState,
    pub time_us: u64,
}

impl KeyEvent {
    /// Construct a key-down transition.
    #[must_use]
    pub const fn down(usage: KeyUsage, time_us: u64) -> Self {
        Self {
            usage,
            state: EventState::Down,
            time_us,
        }
    }

    /// Construct a key-up transition.
    #[must_use]
    pub const fn up(usage: KeyUsage, time_us: u64) -> Self {
        Self {
            usage,
            state: EventState::Up,
            time_us,
        }
    }
}

/// A rejected transition that did not enter output state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApplyAnomaly {
    DuplicateDown,
    OrphanUp,
    TapWhilePressed,
    TimeRegression { previous_us: u64, actual_us: u64 },
}

/// Result of applying one requested transition or Core-28 tap.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ApplyResult {
    pub events: Vec<KeyEvent>,
    pub anomaly: Option<ApplyAnomaly>,
}

/// Fault categories that force the output state to neutral.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FaultReason {
    DecoderReset,
    TransportDisconnect,
    MalformedFrame,
    WatchdogTimeout,
    OutputFailure,
    RunOwnershipRevoked,
    ProcessInterrupt,
    ProcessTerminate,
}

/// Deterministic all-up events produced for a fault.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FaultRelease {
    pub reason: FaultReason,
    pub events: Vec<KeyEvent>,
}

/// Owns the canonical set of keys currently considered down.
///
/// Core-28 calls [`Self::tap`], which returns an immediate down/up pair and
/// never leaves state behind. The lower-level [`Self::apply`] method exists so
/// later hold and modifier gates can exercise the same state machine.
#[derive(Clone, Debug, Default)]
pub struct KeyStateMachine {
    pressed: BTreeSet<KeyUsage>,
    last_time_us: Option<u64>,
}

impl KeyStateMachine {
    /// Construct a neutral key state.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            pressed: BTreeSet::new(),
            last_time_us: None,
        }
    }

    /// Return the exact currently pressed usage set in stable order.
    #[must_use]
    pub const fn pressed(&self) -> &BTreeSet<KeyUsage> {
        &self.pressed
    }

    /// Apply one requested transition.
    ///
    /// Duplicate down and orphan up requests are suppressed. A time regression
    /// releases every pressed key at the last accepted time and does not apply
    /// the regressing request.
    pub fn apply(&mut self, event: KeyEvent) -> ApplyResult {
        if let Some(previous_us) = self.last_time_us
            && event.time_us < previous_us
        {
            return ApplyResult {
                events: self.release_all_at(previous_us),
                anomaly: Some(ApplyAnomaly::TimeRegression {
                    previous_us,
                    actual_us: event.time_us,
                }),
            };
        }
        self.last_time_us = Some(event.time_us);

        match event.state {
            EventState::Down if self.pressed.insert(event.usage) => ApplyResult {
                events: vec![event],
                anomaly: None,
            },
            EventState::Down => ApplyResult {
                events: vec![],
                anomaly: Some(ApplyAnomaly::DuplicateDown),
            },
            EventState::Up if self.pressed.remove(&event.usage) => ApplyResult {
                events: vec![event],
                anomaly: None,
            },
            EventState::Up => ApplyResult {
                events: vec![],
                anomaly: Some(ApplyAnomaly::OrphanUp),
            },
        }
    }

    /// Convert one Core-28 decision to an immediate down/up pair.
    ///
    /// Core-28 does not support holds or rollover. Any pre-existing down state
    /// is therefore a fault: it is released and the new tap is not applied.
    pub fn tap(&mut self, usage: KeyUsage, time_us: u64) -> ApplyResult {
        if !self.pressed.is_empty() {
            let release_time = self.last_time_us.map_or(time_us, |last| last.max(time_us));
            return ApplyResult {
                events: self.release_all_at(release_time),
                anomaly: Some(ApplyAnomaly::TapWhilePressed),
            };
        }

        let down = self.apply(KeyEvent::down(usage, time_us));
        if down.anomaly.is_some() {
            return down;
        }
        let up = self.apply(KeyEvent::up(usage, time_us));
        let mut events = down.events;
        events.extend(up.events);
        ApplyResult {
            events,
            anomaly: up.anomaly,
        }
    }

    /// Force the state to all-up and return every required release event.
    pub fn fault(&mut self, reason: FaultReason, time_us: u64) -> FaultRelease {
        let release_time = self.last_time_us.map_or(time_us, |last| last.max(time_us));
        FaultRelease {
            reason,
            events: self.release_all_at(release_time),
        }
    }

    fn release_all_at(&mut self, time_us: u64) -> Vec<KeyEvent> {
        let events = std::mem::take(&mut self.pressed)
            .into_iter()
            .map(|usage| KeyEvent::up(usage, time_us))
            .collect();
        self.last_time_us = Some(time_us);
        events
    }
}

/// Receipt produced after one adapter accepts a key event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OutputReceipt {
    pub event: KeyEvent,
    pub ordinal: u64,
}

/// The one seam between canonical key events and a concrete output target.
pub trait OutputAdapter {
    type Error;

    /// Emit one already validated event.
    ///
    /// # Errors
    ///
    /// Returns the adapter's concrete output failure.
    fn emit(&mut self, event: KeyEvent) -> Result<OutputReceipt, Self::Error>;

    /// Invoke the adapter's independent all-up recovery primitive.
    ///
    /// # Errors
    ///
    /// Returns the adapter's concrete recovery failure.
    fn release_all(&mut self) -> Result<(), Self::Error>;
}

/// Emit an ordered event slice through one adapter.
///
/// # Errors
///
/// Returns the first adapter error. Callers must then force state and adapter
/// all-up recovery instead of retrying the failed event blindly.
pub fn emit_events<A: OutputAdapter>(
    adapter: &mut A,
    events: &[KeyEvent],
) -> Result<Vec<OutputReceipt>, A::Error> {
    events
        .iter()
        .copied()
        .map(|event| adapter.emit(event))
        .collect()
}

/// Opaque authority for one explicitly armed output run.
///
/// A lease belongs to one [`OutputSafetySession`]. Source loss, run revocation,
/// output failure, or shutdown revokes it. This is not evidence of desktop
/// focus ownership; the adapter still follows the operating system's route.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OutputRunLease {
    session_id: u64,
    revision: u64,
}

/// The independently watched source hands required to arm one output run.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequiredSourceTopology {
    One(Hand),
    TwoHands,
}

impl RequiredSourceTopology {
    fn contains(self, hand: Hand) -> bool {
        match self {
            Self::One(required) => hand == required,
            Self::TwoHands => true,
        }
    }

    fn all_connected(self, connected: &[bool; 2]) -> bool {
        match self {
            Self::One(hand) => connected[hand.index()],
            Self::TwoHands => connected.iter().all(|value| *value),
        }
    }
}

/// Planned process-stop signals understood by the output lifecycle.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShutdownSignal {
    Interrupt,
    Terminate,
}

/// A lifecycle condition that prevents a transition from reaching output.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutputGateError {
    UnexpectedSource,
    SourcesUnavailable,
    RunAlreadyOwned,
    StaleLease,
    SessionStopped,
    LeaseExhausted,
}

/// Evidence that requires a real host/system transition outside unit and
/// exclusively grabbed `uinput` tests.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutputSystemTestGate {
    /// Real host sleep/wake/lock in every key state.
    HostSuspendResume,
    /// Two real application targets before, during, and after focus changes.
    DesktopFocusRouting,
    /// Application callback/state timing for faults where cleanup can run.
    RecoverableFaultApplicationAllUpDeadline,
    /// Application or OS neutral timing after cleanup cannot run.
    AbruptProcessDeathAllUpDeadline,
}

/// Safety claims deliberately left to a physical or host-system test lane.
pub const OUTPUT_SYSTEM_TEST_GATES: [OutputSystemTestGate; 4] = [
    OutputSystemTestGate::HostSuspendResume,
    OutputSystemTestGate::DesktopFocusRouting,
    OutputSystemTestGate::RecoverableFaultApplicationAllUpDeadline,
    OutputSystemTestGate::AbruptProcessDeathAllUpDeadline,
];

/// A state, gate, or adapter failure after neutral recovery was attempted.
#[derive(Debug, PartialEq)]
pub enum OutputSafetyError<E> {
    Gate(OutputGateError),
    State(ApplyAnomaly),
    Adapter {
        event: Option<KeyEvent>,
        cause: E,
        recovery_error: Option<E>,
    },
}

/// Source and run lifecycle around one canonical output adapter.
///
/// Construction invokes the adapter's independent all-up primitive before any
/// source can arm output. This gives a fresh process/session a neutral start
/// even when the adapter object survived a supervising runtime restart.
pub struct OutputSafetySession<A: OutputAdapter> {
    output: A,
    keys: KeyStateMachine,
    required_sources: RequiredSourceTopology,
    sources_connected: [bool; 2],
    session_id: u64,
    next_lease_revision: u64,
    active_lease: Option<OutputRunLease>,
    stopped: bool,
}

impl<A: OutputAdapter> OutputSafetySession<A> {
    /// Start disarmed and force the concrete adapter to a neutral state.
    ///
    /// # Errors
    ///
    /// Returns the adapter error when neutral startup cannot be established.
    pub fn start(
        mut output: A,
        required_sources: RequiredSourceTopology,
    ) -> Result<Self, A::Error> {
        output.release_all()?;
        Ok(Self {
            output,
            keys: KeyStateMachine::new(),
            required_sources,
            sources_connected: [false; 2],
            session_id: NEXT_OUTPUT_SESSION_ID.fetch_add(1, Ordering::Relaxed),
            next_lease_revision: 1,
            active_lease: None,
            stopped: false,
        })
    }

    /// Mark one required source live.
    ///
    /// # Errors
    ///
    /// Rejects a hand outside this session's declared topology.
    pub fn source_connected(&mut self, source: Hand) -> Result<(), OutputGateError> {
        if !self.required_sources.contains(source) {
            return Err(OutputGateError::UnexpectedSource);
        }
        self.sources_connected[source.index()] = true;
        Ok(())
    }

    /// Acquire authority for this explicit run.
    ///
    /// # Errors
    ///
    /// Fails while a required source is absent, another run owns the session,
    /// the process is stopping, or the monotonic revision space is exhausted.
    pub fn acquire_run_ownership(&mut self) -> Result<OutputRunLease, OutputGateError> {
        if self.stopped {
            return Err(OutputGateError::SessionStopped);
        }
        if !self.required_sources.all_connected(&self.sources_connected) {
            return Err(OutputGateError::SourcesUnavailable);
        }
        if self.active_lease.is_some() {
            return Err(OutputGateError::RunAlreadyOwned);
        }
        let next = self
            .next_lease_revision
            .checked_add(1)
            .ok_or(OutputGateError::LeaseExhausted)?;
        let lease = OutputRunLease {
            session_id: self.session_id,
            revision: self.next_lease_revision,
        };
        self.next_lease_revision = next;
        self.active_lease = Some(lease);
        Ok(lease)
    }

    /// Apply one lower-level key transition under the active run lease.
    ///
    /// # Errors
    ///
    /// Rejects stale authority and state anomalies. Any state or adapter fault
    /// revokes the lease and invokes independent all-up recovery.
    pub fn apply(
        &mut self,
        lease: OutputRunLease,
        event: KeyEvent,
    ) -> Result<ApplyResult, OutputSafetyError<A::Error>> {
        self.require_active_lease(lease)
            .map_err(OutputSafetyError::Gate)?;
        let applied = self.keys.apply(event);
        self.finish_apply(applied, event.time_us)
    }

    /// Emit one Core-28 immediate down/up pair under the active run lease.
    ///
    /// # Errors
    ///
    /// Rejects stale authority and state anomalies. Any state or adapter fault
    /// revokes the lease and invokes independent all-up recovery.
    pub fn tap(
        &mut self,
        lease: OutputRunLease,
        usage: KeyUsage,
        time_us: u64,
    ) -> Result<ApplyResult, OutputSafetyError<A::Error>> {
        self.require_active_lease(lease)
            .map_err(OutputSafetyError::Gate)?;
        let applied = self.keys.tap(usage, time_us);
        self.finish_apply(applied, time_us)
    }

    /// Revoke output when a required independently watched source disappears.
    ///
    /// # Errors
    ///
    /// Returns the first adapter recovery error after software state is clear.
    pub fn source_lost(
        &mut self,
        source: Hand,
        time_us: u64,
    ) -> Result<FaultRelease, OutputSafetyError<A::Error>> {
        if !self.required_sources.contains(source) {
            return Err(OutputSafetyError::Gate(OutputGateError::UnexpectedSource));
        }
        self.sources_connected[source.index()] = false;
        self.force_neutral(FaultReason::TransportDisconnect, time_us)
    }

    /// Revoke matching explicit run ownership.
    ///
    /// # Errors
    ///
    /// Stale authority cannot revoke the current run. Adapter errors
    /// are returned after software state is clear and recovery was attempted.
    pub fn revoke_run_ownership(
        &mut self,
        lease: OutputRunLease,
        time_us: u64,
    ) -> Result<FaultRelease, OutputSafetyError<A::Error>> {
        self.require_active_lease(lease)
            .map_err(OutputSafetyError::Gate)?;
        self.force_neutral(FaultReason::RunOwnershipRevoked, time_us)
    }

    /// Stop permanently on SIGINT or SIGTERM semantics and force all-up.
    ///
    /// Signal registration belongs at the process boundary. That boundary must
    /// call this method while it can still execute; SIGKILL cannot run cleanup.
    ///
    /// # Errors
    ///
    /// Returns the first adapter recovery error after software state is clear.
    pub fn shutdown(
        &mut self,
        signal: ShutdownSignal,
        time_us: u64,
    ) -> Result<FaultRelease, OutputSafetyError<A::Error>> {
        self.stopped = true;
        self.sources_connected = [false; 2];
        let reason = match signal {
            ShutdownSignal::Interrupt => FaultReason::ProcessInterrupt,
            ShutdownSignal::Terminate => FaultReason::ProcessTerminate,
        };
        self.force_neutral(reason, time_us)
    }

    /// Force neutral for a decoder/runtime fault and revoke run ownership.
    ///
    /// The runtime remains restartable unless adapter recovery fails. Source
    /// connectivity stays unchanged; the caller must explicitly reacquire run
    /// ownership after its own evidence gates recover.
    ///
    /// # Errors
    ///
    /// Returns adapter recovery failure after software state is clear.
    pub(crate) fn force_neutral_for_runtime(
        &mut self,
        reason: FaultReason,
        time_us: u64,
    ) -> Result<FaultRelease, OutputSafetyError<A::Error>> {
        self.force_neutral(reason, time_us)
    }

    /// Return the concrete adapter for evidence inspection.
    #[must_use]
    pub const fn output(&self) -> &A {
        &self.output
    }

    /// Consume the lifecycle and return its already neutralized or still-live
    /// adapter. A supervising restart passes it to [`Self::start`], which
    /// performs the required independent all-up before rearming.
    #[must_use]
    pub fn into_output(self) -> A {
        self.output
    }

    /// Return the canonical software pressed-key set.
    #[must_use]
    pub const fn pressed(&self) -> &BTreeSet<KeyUsage> {
        self.keys.pressed()
    }

    /// Return whether a current run lease can emit transitions.
    #[must_use]
    pub const fn is_run_owned(&self) -> bool {
        self.active_lease.is_some() && !self.stopped
    }
}

impl<A: OutputAdapter> OutputSafetySession<A> {
    fn finish_apply(
        &mut self,
        applied: ApplyResult,
        time_us: u64,
    ) -> Result<ApplyResult, OutputSafetyError<A::Error>> {
        if let Some(anomaly) = applied.anomaly {
            self.active_lease = None;
            let mut recovery_events = applied.events;
            recovery_events.extend(self.keys.fault(FaultReason::DecoderReset, time_us).events);
            self.emit_recovery_events(&recovery_events)?;
            return Err(OutputSafetyError::State(anomaly));
        }
        self.emit_applied_events(&applied.events, time_us)?;
        Ok(applied)
    }

    fn require_active_lease(&self, lease: OutputRunLease) -> Result<(), OutputGateError> {
        if self.stopped {
            return Err(OutputGateError::SessionStopped);
        }
        if self.active_lease == Some(lease) {
            Ok(())
        } else {
            Err(OutputGateError::StaleLease)
        }
    }

    fn emit_applied_events(
        &mut self,
        events: &[KeyEvent],
        time_us: u64,
    ) -> Result<(), OutputSafetyError<A::Error>> {
        for event in events {
            if let Err(cause) = self.output.emit(*event) {
                self.active_lease = None;
                self.stopped = true;
                let _ = self.keys.fault(FaultReason::OutputFailure, time_us);
                let recovery_error = self.output.release_all().err();
                return Err(OutputSafetyError::Adapter {
                    event: Some(*event),
                    cause,
                    recovery_error,
                });
            }
        }
        Ok(())
    }

    fn emit_recovery_events(
        &mut self,
        events: &[KeyEvent],
    ) -> Result<(), OutputSafetyError<A::Error>> {
        for event in events {
            if let Err(cause) = self.output.emit(*event) {
                self.stopped = true;
                let recovery_error = self.output.release_all().err();
                return Err(OutputSafetyError::Adapter {
                    event: Some(*event),
                    cause,
                    recovery_error,
                });
            }
        }
        self.output.release_all().map_err(|cause| {
            self.stopped = true;
            OutputSafetyError::Adapter {
                event: None,
                cause,
                recovery_error: None,
            }
        })
    }

    fn force_neutral(
        &mut self,
        reason: FaultReason,
        time_us: u64,
    ) -> Result<FaultRelease, OutputSafetyError<A::Error>> {
        self.active_lease = None;
        let release = self.keys.fault(reason, time_us);
        self.emit_recovery_events(&release.events)?;
        Ok(release)
    }
}

/// Deterministic in-memory adapter used by replay and contract tests.
///
/// This is a real adapter at the output seam, not a mock of the state machine.
#[derive(Clone, Debug, Default)]
pub struct RecordingOutput {
    events: Vec<KeyEvent>,
    release_all_calls: u64,
}

impl RecordingOutput {
    /// Construct an empty recording adapter.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            events: Vec::new(),
            release_all_calls: 0,
        }
    }

    /// Return all accepted events in exact order.
    #[must_use]
    pub fn events(&self) -> &[KeyEvent] {
        &self.events
    }

    /// Return the number of independent adapter all-up requests.
    #[must_use]
    pub const fn release_all_calls(&self) -> u64 {
        self.release_all_calls
    }
}

impl OutputAdapter for RecordingOutput {
    type Error = Infallible;

    fn emit(&mut self, event: KeyEvent) -> Result<OutputReceipt, Self::Error> {
        self.events.push(event);
        Ok(OutputReceipt {
            event,
            ordinal: u64::try_from(self.events.len() - 1)
                .expect("recording event count fits in u64"),
        })
    }

    fn release_all(&mut self) -> Result<(), Self::Error> {
        self.release_all_calls += 1;
        Ok(())
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::{
        collections::BTreeSet,
        ffi::CStr,
        fs, io,
        os::unix::fs::MetadataExt,
        path::{Path, PathBuf},
        thread,
        time::{Duration, Instant},
    };

    use evdev::{
        AttributeSet, BusType, Device, EventSummary, InputId, KeyCode, KeyEvent as EvdevKeyEvent,
        uinput::VirtualDevice,
    };

    use super::{EventState, KeyEvent, OutputAdapter, OutputReceipt};
    use crate::{
        clock::percentile,
        layout::{KeyUsage, all_slots},
    };

    /// Linux kernel `uinput` adapter for the exact Core-28 capability set.
    ///
    /// Device creation through `evdev` requires read/write access to
    /// `/dev/uinput`. The decoder process should receive that narrow access
    /// through host policy rather than running as root.
    #[derive(Debug)]
    pub struct LinuxUinputOutput {
        device: VirtualDevice,
        exclusive_reader: Option<Device>,
        pressed: BTreeSet<KeyUsage>,
        ordinal: u64,
    }

    /// Result of an exclusively grabbed kernel round-trip probe.
    #[derive(Clone, Debug, Eq, PartialEq)]
    pub struct UinputProbeReport {
        pub evidence_label: &'static str,
        pub device_node: PathBuf,
        pub observed_values: [i32; 2],
        pub stuck_keys: usize,
        pub access: UinputAccessReport,
    }

    /// Non-root process and device-node evidence captured by a kernel probe.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub struct UinputAccessReport {
        pub effective_uid: u32,
        pub effective_gid: u32,
        pub uinput_mode: u32,
        pub uinput_uid: u32,
        pub uinput_gid: u32,
        pub event_node_mode: u32,
        pub event_node_uid: u32,
        pub event_node_gid: u32,
    }

    /// Result of the fixed, exclusively grabbed kernel output stress run.
    #[derive(Clone, Debug, Eq, PartialEq)]
    pub struct UinputStressReport {
        pub evidence_label: &'static str,
        pub device_node: PathBuf,
        pub tap_pairs: u64,
        pub fault_release_cycles: u64,
        pub observed_key_events: u64,
        pub round_trip_p95_us: u64,
        pub round_trip_p99_us: u64,
        pub stuck_keys: usize,
    }

    /// Result observed by a native userspace evdev consumer.
    ///
    /// This is stronger than inspecting the adapter's mirror, but it is not a
    /// desktop focus, compositor, IME, suspend/resume, or physical-input test.
    #[derive(Clone, Debug, Eq, PartialEq)]
    pub struct UinputApplicationReport {
        pub application_evidence_label: &'static str,
        pub kernel_readback_evidence_label: &'static str,
        pub device_node: PathBuf,
        pub advertised_usages: u64,
        pub tap_pairs: u64,
        pub application_observed_events: u64,
        pub application_stuck_keys: usize,
        pub kernel_readback_stuck_keys: usize,
        pub application_round_trip_p95_us: u64,
        pub application_round_trip_p99_us: u64,
    }

    const KERNEL_READBACK_EVIDENCE: &str = "linux-kernel-event-node-readback";
    const NATIVE_APPLICATION_EVIDENCE: &str = "native-userspace-evdev-consumer";
    const CORE28_PHYSICAL_PATH: &CStr = c"invisible-keyboard/core28";
    const EVENT_NODE_READY_TIMEOUT: Duration = Duration::from_secs(2);
    const EVENT_NODE_RETRY_INTERVAL: Duration = Duration::from_millis(5);

    impl LinuxUinputOutput {
        /// Create and register one virtual Core-28 keyboard.
        ///
        /// # Errors
        ///
        /// Returns the kernel or device-configuration error, most commonly
        /// `PermissionDenied` when `/dev/uinput` policy has not been installed.
        pub fn new(name: &str) -> io::Result<Self> {
            let mut keys = AttributeSet::<KeyCode>::new();
            for usage in all_slots().iter().filter_map(|slot| slot.usage) {
                keys.insert(key_code(usage));
            }
            let device = VirtualDevice::builder()?
                .name(name)
                .input_id(core28_input_id())
                .with_phys(CORE28_PHYSICAL_PATH)?
                .with_keys(&keys)?
                .build()?;
            Ok(Self {
                device,
                exclusive_reader: None,
                pressed: BTreeSet::new(),
                ordinal: 0,
            })
        }

        /// Return the adapter's independent pressed-key mirror.
        #[must_use]
        pub const fn pressed(&self) -> &BTreeSet<KeyUsage> {
            &self.pressed
        }

        /// Return the registered event node for a separately opened consumer.
        ///
        /// # Errors
        ///
        /// Returns node enumeration errors or `NotFound` when registration did
        /// not produce an event node.
        pub fn device_node(&mut self) -> io::Result<PathBuf> {
            self.device
                .enumerate_dev_nodes_blocking()?
                .next()
                .transpose()?
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "uinput event node missing"))
        }

        /// Open and exclusively grab this adapter's event node.
        ///
        /// The adapter owns the reader so its all-up `Drop` recovery runs while
        /// the grab is still active. This keeps probe events away from desktop
        /// applications, including on an early-return error path.
        ///
        /// # Errors
        ///
        /// Returns node discovery, open, or exclusive-grab errors. A second
        /// grab request fails with `AlreadyExists`.
        pub fn grab_event_node(&mut self) -> io::Result<PathBuf> {
            if self.exclusive_reader.is_some() {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "uinput event node is already grabbed",
                ));
            }
            let device_node = self.device_node()?;
            let mut reader = open_event_node_when_ready(&device_node)?;
            validate_core28_device(&reader)?;
            reader.grab()?;
            self.exclusive_reader = Some(reader);
            Ok(device_node)
        }

        /// Count the keys down in the exclusively grabbed kernel device.
        ///
        /// # Errors
        ///
        /// Returns `NotConnected` before [`Self::grab_event_node`] or the
        /// kernel state-query error.
        pub fn grabbed_pressed_key_count(&self) -> io::Result<usize> {
            self.exclusive_reader
                .as_ref()
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::NotConnected,
                        "uinput event node is not grabbed",
                    )
                })?
                .get_key_state()
                .map(|state| state.iter().count())
        }

        fn read_grabbed_key_events_into(
            &mut self,
            observed: &mut Vec<(KeyCode, i32)>,
        ) -> io::Result<()> {
            let reader = self.exclusive_reader.as_mut().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotConnected,
                    "uinput event node is not grabbed",
                )
            })?;
            read_key_events_into(reader, observed)
        }
    }

    impl OutputAdapter for LinuxUinputOutput {
        type Error = io::Error;

        fn emit(&mut self, event: KeyEvent) -> Result<OutputReceipt, Self::Error> {
            let value = match event.state {
                EventState::Down => 1,
                EventState::Up => 0,
            };
            let kernel_event = *EvdevKeyEvent::new(key_code(event.usage), value);
            // Record a conservative down state before writing. If the kernel
            // accepts a partial write and reports an error, the caller's
            // independent release_all path must still send the matching up.
            match event.state {
                EventState::Down => {
                    self.pressed.insert(event.usage);
                    self.device.emit(&[kernel_event])?;
                }
                EventState::Up => {
                    self.device.emit(&[kernel_event])?;
                    self.pressed.remove(&event.usage);
                }
            }
            let receipt = OutputReceipt {
                event,
                ordinal: self.ordinal,
            };
            self.ordinal = self.ordinal.saturating_add(1);
            Ok(receipt)
        }

        fn release_all(&mut self) -> Result<(), Self::Error> {
            if self.pressed.is_empty() {
                return Ok(());
            }
            let releases = self
                .pressed
                .iter()
                .copied()
                .map(|usage| *EvdevKeyEvent::new(key_code(usage), 0))
                .collect::<Vec<_>>();
            self.device.emit(&releases)?;
            self.pressed.clear();
            Ok(())
        }
    }

    impl Drop for LinuxUinputOutput {
        fn drop(&mut self) {
            // Drop is a last safety net, not the normal recovery path. The
            // explicit watchdog path can observe and report an all-up error.
            let _ = self.release_all();
            // Close and ungrab the event reader before VirtualDevice destroys
            // the kernel device. evdev otherwise reports a spurious ENODEV
            // while its reader is being dropped.
            self.exclusive_reader.take();
        }
    }

    const fn key_code(usage: KeyUsage) -> KeyCode {
        match usage {
            KeyUsage::A => KeyCode::KEY_A,
            KeyUsage::B => KeyCode::KEY_B,
            KeyUsage::C => KeyCode::KEY_C,
            KeyUsage::D => KeyCode::KEY_D,
            KeyUsage::E => KeyCode::KEY_E,
            KeyUsage::F => KeyCode::KEY_F,
            KeyUsage::G => KeyCode::KEY_G,
            KeyUsage::H => KeyCode::KEY_H,
            KeyUsage::I => KeyCode::KEY_I,
            KeyUsage::J => KeyCode::KEY_J,
            KeyUsage::K => KeyCode::KEY_K,
            KeyUsage::L => KeyCode::KEY_L,
            KeyUsage::M => KeyCode::KEY_M,
            KeyUsage::N => KeyCode::KEY_N,
            KeyUsage::O => KeyCode::KEY_O,
            KeyUsage::P => KeyCode::KEY_P,
            KeyUsage::Q => KeyCode::KEY_Q,
            KeyUsage::R => KeyCode::KEY_R,
            KeyUsage::S => KeyCode::KEY_S,
            KeyUsage::T => KeyCode::KEY_T,
            KeyUsage::U => KeyCode::KEY_U,
            KeyUsage::V => KeyCode::KEY_V,
            KeyUsage::W => KeyCode::KEY_W,
            KeyUsage::X => KeyCode::KEY_X,
            KeyUsage::Y => KeyCode::KEY_Y,
            KeyUsage::Z => KeyCode::KEY_Z,
            KeyUsage::Space => KeyCode::KEY_SPACE,
            KeyUsage::Backspace => KeyCode::KEY_BACKSPACE,
        }
    }

    /// Prove one down/up pair across the running kernel without exposing it to
    /// the desktop or any other reader.
    ///
    /// The function grabs the new event node before it emits the probe key.
    ///
    /// # Errors
    ///
    /// Returns device creation, node discovery, exclusive-grab, event write,
    /// event read, or exact-round-trip validation errors.
    pub fn probe_core28_uinput() -> io::Result<UinputProbeReport> {
        let (effective_uid, effective_gid) = effective_process_ids()?;
        let uinput_metadata = fs::metadata("/dev/uinput")?;
        let mut adapter = LinuxUinputOutput::new("Invisible Keyboard Core-28 Probe")?;
        let device_node = adapter.grab_event_node()?;
        let event_node_metadata = fs::metadata(&device_node)?;

        adapter.emit(KeyEvent::down(KeyUsage::A, 1))?;
        adapter.emit(KeyEvent::up(KeyUsage::A, 2))?;
        let mut observed = Vec::with_capacity(2);
        adapter.read_grabbed_key_events_into(&mut observed)?;
        require_key_events(&observed, &[(KeyCode::KEY_A, 1), (KeyCode::KEY_A, 0)])?;
        let observed_values = [observed[0].1, observed[1].1];
        adapter.release_all()?;
        let stuck_keys = adapter.grabbed_pressed_key_count()?;

        if observed_values != [1, 0] || stuck_keys != 0 || !adapter.pressed().is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "uinput A round trip produced {observed_values:?}, {stuck_keys} kernel keys, and {} adapter keys; expected [1, 0] and neutral state",
                    adapter.pressed().len()
                ),
            ));
        }
        Ok(UinputProbeReport {
            evidence_label: KERNEL_READBACK_EVIDENCE,
            device_node,
            observed_values,
            stuck_keys,
            access: UinputAccessReport {
                effective_uid,
                effective_gid,
                uinput_mode: permission_mode(&uinput_metadata),
                uinput_uid: uinput_metadata.uid(),
                uinput_gid: uinput_metadata.gid(),
                event_node_mode: permission_mode(&event_node_metadata),
                event_node_uid: event_node_metadata.uid(),
                event_node_gid: event_node_metadata.gid(),
            },
        })
    }

    /// Stress the actual Linux kernel output path without exposing events to
    /// the desktop.
    ///
    /// The event node is exclusively grabbed before the first event. The run
    /// verifies 10,000 exact A down/up pairs, ten independent all-up recovery
    /// cycles, and the kernel's final key state. Per-pair round-trip timing
    /// includes adapter writes and reading the events back from the kernel.
    ///
    /// # Errors
    ///
    /// Returns a kernel I/O error or an exact event/state mismatch.
    pub fn stress_core28_uinput() -> io::Result<UinputStressReport> {
        const TAP_PAIRS: u64 = 10_000;
        const FAULT_RELEASE_CYCLES: u64 = 10;

        let mut adapter = LinuxUinputOutput::new("Invisible Keyboard Core-28 Stress")?;
        let device_node = adapter.grab_event_node()?;

        let capacity = usize::try_from(TAP_PAIRS).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "uinput stress pair count exceeds this platform",
            )
        })?;
        let mut round_trip_us = Vec::with_capacity(capacity);
        let mut observed_key_events = 0_u64;
        let mut observed = Vec::with_capacity(2);
        for pair in 0..TAP_PAIRS {
            let started = Instant::now();
            adapter.emit(KeyEvent::down(KeyUsage::A, pair.saturating_mul(2)))?;
            adapter.emit(KeyEvent::up(
                KeyUsage::A,
                pair.saturating_mul(2).saturating_add(1),
            ))?;
            adapter.read_grabbed_key_events_into(&mut observed)?;
            round_trip_us.push(u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX));
            require_key_events(&observed, &[(KeyCode::KEY_A, 1), (KeyCode::KEY_A, 0)])?;
            observed_key_events = observed_key_events
                .saturating_add(u64::try_from(observed.len()).unwrap_or(u64::MAX));
        }

        for cycle in 0..FAULT_RELEASE_CYCLES {
            let time_us = TAP_PAIRS
                .saturating_mul(2)
                .saturating_add(cycle.saturating_mul(2));
            adapter.emit(KeyEvent::down(KeyUsage::A, time_us))?;
            adapter.read_grabbed_key_events_into(&mut observed)?;
            require_key_events(&observed, &[(KeyCode::KEY_A, 1)])?;
            observed_key_events = observed_key_events
                .saturating_add(u64::try_from(observed.len()).unwrap_or(u64::MAX));

            adapter.release_all()?;
            adapter.read_grabbed_key_events_into(&mut observed)?;
            require_key_events(&observed, &[(KeyCode::KEY_A, 0)])?;
            observed_key_events = observed_key_events
                .saturating_add(u64::try_from(observed.len()).unwrap_or(u64::MAX));
        }

        let stuck_keys = adapter.grabbed_pressed_key_count()?;
        if stuck_keys != 0 || !adapter.pressed().is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "uinput stress ended with {stuck_keys} kernel keys and {} adapter keys pressed",
                    adapter.pressed().len()
                ),
            ));
        }

        if round_trip_us.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "uinput stress recorded no timings",
            ));
        }
        let round_trip_p95_us = percentile(&mut round_trip_us, 95);
        let round_trip_p99_us = percentile(&mut round_trip_us, 99);

        Ok(UinputStressReport {
            evidence_label: KERNEL_READBACK_EVIDENCE,
            device_node,
            tap_pairs: TAP_PAIRS,
            fault_release_cycles: FAULT_RELEASE_CYCLES,
            observed_key_events,
            round_trip_p95_us,
            round_trip_p99_us,
            stuck_keys,
        })
    }

    /// Send every Core-28 pair to an exclusively grabbed native userspace
    /// evdev consumer and compare its state with independent kernel readback.
    ///
    /// The exclusive grab keeps this proof harmless: no desktop application or
    /// compositor receives the generated keys. The result must therefore be
    /// labeled as a native evdev consumer, not as ordinary focused-app proof.
    ///
    /// # Errors
    ///
    /// Returns device creation, node discovery, exclusive-grab, event write,
    /// userspace observation, or final-state validation errors.
    pub fn observe_core28_uinput_application() -> io::Result<UinputApplicationReport> {
        const TAP_PAIRS: u64 = 10_000;

        let mut adapter = LinuxUinputOutput::new("Invisible Keyboard Core-28 Native Oracle")?;
        let device_node = adapter.grab_event_node()?;
        let mut application = NativeEvdevApplication::default();
        let usages = all_slots()
            .iter()
            .filter_map(|slot| slot.usage)
            .collect::<Vec<_>>();
        let mut round_trip_us = Vec::with_capacity(usize::try_from(TAP_PAIRS).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "native oracle pair count exceeds this platform",
            )
        })?);
        let mut observed = Vec::with_capacity(2);

        for (pair, usage) in (0..TAP_PAIRS).zip(usages.iter().copied().cycle()) {
            let started = Instant::now();
            adapter.emit(KeyEvent::down(usage, pair.saturating_mul(2)))?;
            adapter.emit(KeyEvent::up(
                usage,
                pair.saturating_mul(2).saturating_add(1),
            ))?;
            adapter.read_grabbed_key_events_into(&mut observed)?;
            require_key_events(&observed, &[(key_code(usage), 1), (key_code(usage), 0)])?;
            application.observe(&observed)?;
            round_trip_us.push(u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX));
        }

        adapter.release_all()?;
        let kernel_readback_stuck_keys = adapter.grabbed_pressed_key_count()?;
        if !adapter.pressed().is_empty()
            || !application.pressed.is_empty()
            || kernel_readback_stuck_keys != 0
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "native oracle ended with {} adapter, {} application, and {kernel_readback_stuck_keys} kernel keys pressed",
                    adapter.pressed().len(),
                    application.pressed.len(),
                ),
            ));
        }

        if round_trip_us.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "native oracle recorded no timings",
            ));
        }
        let application_round_trip_p95_us = percentile(&mut round_trip_us, 95);
        let application_round_trip_p99_us = percentile(&mut round_trip_us, 99);

        Ok(UinputApplicationReport {
            application_evidence_label: NATIVE_APPLICATION_EVIDENCE,
            kernel_readback_evidence_label: KERNEL_READBACK_EVIDENCE,
            device_node,
            advertised_usages: u64::try_from(usages.len()).unwrap_or(u64::MAX),
            tap_pairs: TAP_PAIRS,
            application_observed_events: application.observed_events,
            application_stuck_keys: application.pressed.len(),
            kernel_readback_stuck_keys,
            application_round_trip_p95_us,
            application_round_trip_p99_us,
        })
    }

    #[derive(Default)]
    struct NativeEvdevApplication {
        pressed: BTreeSet<KeyUsage>,
        observed_events: u64,
    }

    impl NativeEvdevApplication {
        fn observe(&mut self, events: &[(KeyCode, i32)]) -> io::Result<()> {
            for &(key, value) in events {
                let usage = key_usage(key).ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("native oracle received non-Core-28 key {key:?}"),
                    )
                })?;
                match value {
                    1 if self.pressed.insert(usage) => {}
                    0 if self.pressed.remove(&usage) => {}
                    1 => {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!("native oracle received duplicate down for {usage:?}"),
                        ));
                    }
                    0 => {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!("native oracle received orphan up for {usage:?}"),
                        ));
                    }
                    _ => {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!("native oracle received unsupported value {value}"),
                        ));
                    }
                }
                self.observed_events = self.observed_events.saturating_add(1);
            }
            Ok(())
        }
    }

    fn read_key_events_into(
        reader: &mut Device,
        observed: &mut Vec<(KeyCode, i32)>,
    ) -> io::Result<()> {
        observed.clear();
        observed.extend(
            reader
                .fetch_events()?
                .filter_map(|event| match event.destructure() {
                    EventSummary::Key(_, key, value) => Some((key, value)),
                    _ => None,
                }),
        );
        Ok(())
    }

    fn open_event_node_when_ready(device_node: &Path) -> io::Result<Device> {
        let deadline = Instant::now() + EVENT_NODE_READY_TIMEOUT;
        loop {
            match Device::open(device_node) {
                Ok(device) => return Ok(device),
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied
                    ) && Instant::now() < deadline =>
                {
                    // Sysfs registration precedes udev's ownership and ACL
                    // update. Wait only for those two transient states.
                    thread::sleep(EVENT_NODE_RETRY_INTERVAL);
                }
                Err(error) => return Err(error),
            }
        }
    }

    fn key_usage(key: KeyCode) -> Option<KeyUsage> {
        all_slots()
            .iter()
            .filter_map(|slot| slot.usage)
            .find(|usage| key_code(*usage) == key)
    }

    fn require_key_events(
        observed: &[(KeyCode, i32)],
        expected: &[(KeyCode, i32)],
    ) -> io::Result<()> {
        if observed == expected {
            return Ok(());
        }
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("uinput produced {observed:?}; expected {expected:?}"),
        ))
    }

    fn validate_core28_device(device: &Device) -> io::Result<()> {
        let input_id = device.input_id();
        let expected_input_id = core28_input_id();
        let expected_physical_path = CORE28_PHYSICAL_PATH
            .to_str()
            .expect("Core-28 physical path literal is UTF-8");
        if input_id != expected_input_id || device.physical_path() != Some(expected_physical_path) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "uinput identity was {input_id:?} at {:?}; expected {expected_input_id:?} at {expected_physical_path}",
                    device.physical_path()
                ),
            ));
        }

        let expected = all_slots()
            .iter()
            .filter_map(|slot| slot.usage)
            .map(key_code)
            .collect::<BTreeSet<_>>();
        let actual = device
            .supported_keys()
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "uinput device advertised no key capability set",
                )
            })?
            .iter()
            .collect::<BTreeSet<_>>();
        if actual != expected {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("uinput key capabilities were {actual:?}; expected {expected:?}"),
            ));
        }
        Ok(())
    }

    fn core28_input_id() -> InputId {
        InputId::new(BusType::BUS_VIRTUAL, 0, 0, 1)
    }

    fn effective_process_ids() -> io::Result<(u32, u32)> {
        let status = fs::read_to_string("/proc/self/status")?;
        Ok((
            effective_status_id(&status, "Uid:")?,
            effective_status_id(&status, "Gid:")?,
        ))
    }

    fn effective_status_id(status: &str, label: &str) -> io::Result<u32> {
        status
            .lines()
            .find_map(|line| {
                let mut fields = line.split_ascii_whitespace();
                (fields.next() == Some(label))
                    .then(|| fields.nth(1)?.parse::<u32>().ok())
                    .flatten()
            })
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("/proc/self/status has no effective {label} value"),
                )
            })
    }

    fn permission_mode(metadata: &fs::Metadata) -> u32 {
        metadata.mode() & 0o777
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::layout::CORE28_KEY_COUNT;

        #[test]
        fn all_core28_usages_have_unique_kernel_codes() {
            let usages = all_slots().iter().filter_map(|slot| slot.usage);
            let codes = usages.clone().map(key_code).collect::<BTreeSet<_>>();

            assert_eq!(usages.count(), CORE28_KEY_COUNT);
            assert_eq!(codes.len(), CORE28_KEY_COUNT);
        }
    }
}

#[cfg(target_os = "linux")]
pub use linux::{
    LinuxUinputOutput, UinputAccessReport, UinputApplicationReport, UinputProbeReport,
    UinputStressReport, observe_core28_uinput_application, probe_core28_uinput,
    stress_core28_uinput,
};
