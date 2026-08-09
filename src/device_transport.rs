//! Deterministic host-side merging for independent per-hand USB CDC streams.

use std::{collections::BTreeMap, error::Error, fmt};

use crate::{
    layout::Hand,
    protocol::{
        ALL_SENSORS_VALID, FrameError, FrameIdentity, QuiesceReceipt, QuiesceRequest, SensorFrame,
        decode_frame,
    },
    transport::{
        ControllerRecord, ControllerRecordError, TransportCandidate, TransportDecoder,
        decode_controller_record,
    },
};

/// Default time allowed for reader-thread delivery reorder.
///
/// Reader threads timestamp bytes immediately after `read`, before publishing
/// them to the shared channel. A loaded host can delay that publication even
/// though the receipt itself is valid. This explicit bound preserves the real
/// receipt time, remains below the 100 ms source-silence timeout, and still
/// rejects unbounded scheduler stalls.
pub const DEFAULT_MERGE_LATENESS_NS: u64 = 50_000_000;

/// Default interval without bytes before a connected source is reported silent.
pub const DEFAULT_SOURCE_SILENCE_TIMEOUT_NS: u64 = 100_000_000;

/// Default hard limit for candidates waiting behind the merge watermark.
pub const DEFAULT_MAX_PENDING_CANDIDATES: usize = 64;

/// Default time allowed to observe one valid identity frame from each controller.
pub const DEFAULT_IDENTITY_BOOTSTRAP_TIMEOUT_NS: u64 = 2_000_000_000;

/// Default byte bound retained before all declared controller boots are known.
pub const DEFAULT_IDENTITY_BOOTSTRAP_MAX_BYTES: usize = 262_144;

/// Default event bound retained before all declared controller boots are known.
pub const DEFAULT_IDENTITY_BOOTSTRAP_MAX_EVENTS: usize = 4_096;

/// One receipt plus the host observation time of its terminating delimiter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ObservedQuiesceReceipt {
    pub receipt: QuiesceReceipt,
    pub observation_time_ns: u64,
}

/// Evidence retained while one Gate 0A device chunk is dispatched.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Gate0DeviceTransportUpdate {
    pub candidates: Vec<SourcedTransportCandidate>,
    pub receipt: Option<ObservedQuiesceReceipt>,
    pub fault: Option<Gate0DeviceTransportFault>,
}

/// Controller-stream conditions that permanently abort Gate 0A recording.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Gate0DeviceTransportFault {
    InvalidCandidate,
    InvalidControllerRecord(ControllerRecordError),
    IdentityMismatch {
        expected: FrameIdentity,
        actual: FrameIdentity,
    },
    ReceiptBeforeRequest,
    ReceiptBindingMismatch,
    ReceiptWithoutImmediateCleanFrame,
    PostReceiptBytes {
        count: usize,
    },
}

impl fmt::Display for Gate0DeviceTransportFault {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidCandidate => {
                formatter.write_str("Gate 0A controller emitted a malformed or oversized candidate")
            }
            Self::InvalidControllerRecord(error) => {
                write!(formatter, "Gate 0A controller record is invalid: {error}")
            }
            Self::IdentityMismatch { expected, actual } => write!(
                formatter,
                "Gate 0A controller identity changed from {expected:?} to {actual:?}"
            ),
            Self::ReceiptBeforeRequest => {
                formatter.write_str("Gate 0A controller emitted a receipt before the stop request")
            }
            Self::ReceiptBindingMismatch => formatter.write_str(
                "Gate 0A receipt does not match the request identity, session, and start time",
            ),
            Self::ReceiptWithoutImmediateCleanFrame => formatter.write_str(
                "Gate 0A receipt does not immediately follow its clean final sensor frame",
            ),
            Self::PostReceiptBytes { count } => {
                write!(
                    formatter,
                    "Gate 0A controller emitted {count} byte(s) after its receipt"
                )
            }
        }
    }
}

impl Error for Gate0DeviceTransportFault {}

/// API misuse or host-time regression in the one-shot Gate 0A dispatcher.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Gate0DeviceTransportError {
    InvalidExpectedIdentity,
    RequestAlreadyStarted,
    RequestIdentityMismatch,
    ReceiptTimeRegression { previous: u64, actual: u64 },
}

impl fmt::Display for Gate0DeviceTransportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidExpectedIdentity => {
                formatter.write_str("Gate 0A expected identity contains an all-zero field")
            }
            Self::RequestAlreadyStarted => {
                formatter.write_str("Gate 0A stop request was already started")
            }
            Self::RequestIdentityMismatch => {
                formatter.write_str("Gate 0A stop request differs from the locked controller")
            }
            Self::ReceiptTimeRegression { previous, actual } => write!(
                formatter,
                "Gate 0A receipt time regressed from {previous} ns to {actual} ns"
            ),
        }
    }
}

impl Error for Gate0DeviceTransportError {}

/// One-controller, receipt-aware dispatcher for the decisive Gate 0A path.
///
/// This path deliberately does not use the two-source lateness merger. One TTY
/// has a single byte order, and the read call that contains the delimiter owns
/// the receipt observation timestamp. Processing a chunk byte by byte lets the
/// dispatcher reject even one partial byte following a valid receipt.
pub struct Gate0DeviceTransport {
    expected_identity: FrameIdentity,
    decoder: TransportDecoder,
    request: Option<QuiesceRequest>,
    receipt_accepted: bool,
    next_ordinal: u64,
    last_receipt_time_ns: Option<u64>,
    immediate_frame: Option<crate::protocol::SensorFrame>,
}

impl Gate0DeviceTransport {
    /// Start dispatch for one nonzero, boot-locked controller identity.
    pub fn new(expected_identity: FrameIdentity) -> Result<Self, Gate0DeviceTransportError> {
        if expected_identity.controller_id_sha256 == [0; 32] || expected_identity.boot_id == [0; 16]
        {
            return Err(Gate0DeviceTransportError::InvalidExpectedIdentity);
        }
        Ok(Self {
            expected_identity,
            decoder: TransportDecoder::new(),
            request: None,
            receipt_accepted: false,
            next_ordinal: 0,
            last_receipt_time_ns: None,
            immediate_frame: None,
        })
    }

    /// Bind the sole outbound request before its bytes enter the TTY.
    pub fn begin_request(
        &mut self,
        request: QuiesceRequest,
    ) -> Result<(), Gate0DeviceTransportError> {
        if self.request.is_some() {
            return Err(Gate0DeviceTransportError::RequestAlreadyStarted);
        }
        if request.hand != self.expected_identity.hand
            || request.controller_id_sha256 != self.expected_identity.controller_id_sha256
            || request.boot_id != self.expected_identity.boot_id
            || request.session_id == [0; 16]
        {
            return Err(Gate0DeviceTransportError::RequestIdentityMismatch);
        }
        self.request = Some(request);
        // The session request record must precede the final frame. A frame
        // observed during bootstrap cannot satisfy the terminal adjacency.
        self.immediate_frame = None;
        Ok(())
    }

    /// Dispatch one exact read result and retain all pre-fault evidence.
    pub fn push(
        &mut self,
        receipt_time_ns: u64,
        bytes: &[u8],
    ) -> Result<Gate0DeviceTransportUpdate, Gate0DeviceTransportError> {
        if let Some(previous) = self.last_receipt_time_ns
            && receipt_time_ns < previous
        {
            return Err(Gate0DeviceTransportError::ReceiptTimeRegression {
                previous,
                actual: receipt_time_ns,
            });
        }
        self.last_receipt_time_ns = Some(receipt_time_ns);
        if self.receipt_accepted && !bytes.is_empty() {
            return Ok(Gate0DeviceTransportUpdate {
                fault: Some(Gate0DeviceTransportFault::PostReceiptBytes { count: bytes.len() }),
                ..Gate0DeviceTransportUpdate::default()
            });
        }

        let mut update = Gate0DeviceTransportUpdate::default();
        for (index, byte) in bytes.iter().enumerate() {
            if self.receipt_accepted {
                update.fault = Some(Gate0DeviceTransportFault::PostReceiptBytes {
                    count: bytes.len() - index,
                });
                break;
            }
            let candidates = self.decoder.push(std::slice::from_ref(byte));
            let Some(candidate) = candidates.into_iter().next() else {
                continue;
            };
            if let Some(fault) = self.dispatch_candidate(candidate, receipt_time_ns, &mut update) {
                update.fault = Some(fault);
                break;
            }
        }
        Ok(update)
    }

    fn dispatch_candidate(
        &mut self,
        candidate: TransportCandidate,
        receipt_time_ns: u64,
        update: &mut Gate0DeviceTransportUpdate,
    ) -> Option<Gate0DeviceTransportFault> {
        let source_ordinal = self.next_ordinal;
        self.next_ordinal = self.next_ordinal.saturating_add(1);
        let (identity, frame) = candidate_identity(self.expected_identity, &candidate);
        let sourced = SourcedTransportCandidate {
            source: self.expected_identity.hand,
            receipt_time_ns,
            source_ordinal,
            identity,
            candidate,
            frame,
        };
        let TransportCandidate::Decoded(payload) = &sourced.candidate else {
            self.immediate_frame = None;
            update.candidates.push(sourced);
            return Some(Gate0DeviceTransportFault::InvalidCandidate);
        };

        match decode_controller_record(payload) {
            Ok(ControllerRecord::SensorFrame(frame)) => {
                update.candidates.push(sourced);
                if let CandidateIdentity::Mismatch { expected, actual } = identity {
                    self.immediate_frame = None;
                    return Some(Gate0DeviceTransportFault::IdentityMismatch { expected, actual });
                }
                self.immediate_frame = Some(frame);
                None
            }
            Ok(ControllerRecord::QuiesceReceipt(receipt)) => {
                let request = self.request;
                let immediate_frame = self.immediate_frame.take();
                update.receipt = Some(ObservedQuiesceReceipt {
                    receipt,
                    observation_time_ns: receipt_time_ns,
                });
                let Some(request) = request else {
                    return Some(Gate0DeviceTransportFault::ReceiptBeforeRequest);
                };
                if !receipt_matches_request(receipt, request) {
                    return Some(Gate0DeviceTransportFault::ReceiptBindingMismatch);
                }
                let Some(frame) = immediate_frame else {
                    return Some(Gate0DeviceTransportFault::ReceiptWithoutImmediateCleanFrame);
                };
                if frame.flags != 0
                    || frame.valid_sensor_mask != ALL_SENSORS_VALID
                    || frame.sequence != receipt.final_sequence
                {
                    return Some(Gate0DeviceTransportFault::ReceiptWithoutImmediateCleanFrame);
                }
                self.receipt_accepted = true;
                None
            }
            Err(error) => {
                self.immediate_frame = None;
                update.candidates.push(sourced);
                Some(Gate0DeviceTransportFault::InvalidControllerRecord(error))
            }
        }
    }
}

fn receipt_matches_request(receipt: QuiesceReceipt, request: QuiesceRequest) -> bool {
    receipt.hand == request.hand
        && receipt.controller_id_sha256 == request.controller_id_sha256
        && receipt.boot_id == request.boot_id
        && receipt.session_id == request.session_id
        && receipt.request_start_time_ns == request.request_start_time_ns
}

/// Bounds for the pre-manifest controller identity phase.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IdentityBootstrapConfig {
    pub timeout_ns: u64,
    pub max_buffered_bytes: usize,
    pub max_buffered_events: usize,
}

impl Default for IdentityBootstrapConfig {
    fn default() -> Self {
        Self {
            timeout_ns: DEFAULT_IDENTITY_BOOTSTRAP_TIMEOUT_NS,
            max_buffered_bytes: DEFAULT_IDENTITY_BOOTSTRAP_MAX_BYTES,
            max_buffered_events: DEFAULT_IDENTITY_BOOTSTRAP_MAX_EVENTS,
        }
    }
}

#[derive(Clone, Debug)]
struct BootstrapSource {
    expected_controller_id_sha256: [u8; 32],
    decoder: TransportDecoder,
    identity: Option<FrameIdentity>,
}

/// Completed discovery plus every byte event that must enter the raw session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BootstrappedDeviceIdentity {
    pub identities: Vec<FrameIdentity>,
    pub events: Vec<DeviceTransportEvent>,
}

/// Failures that prevent wire identities from locking an acquisition manifest.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IdentityBootstrapError {
    InvalidTopology,
    InvalidConfiguration,
    UndeclaredSource(Hand),
    HandIdentityMismatch { source: Hand, actual: Hand },
    ControllerIdentityMismatch(Hand),
    BootIdentityChanged(Hand),
    SourceDisconnected(Hand),
    BufferCapacityExceeded,
    Timeout,
    Incomplete,
}

impl fmt::Display for IdentityBootstrapError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidTopology => {
                formatter.write_str("identity bootstrap requires one or two unique controllers")
            }
            Self::InvalidConfiguration => {
                formatter.write_str("identity bootstrap bounds must be nonzero")
            }
            Self::UndeclaredSource(source) => {
                write!(
                    formatter,
                    "identity bootstrap source {source:?} is undeclared"
                )
            }
            Self::HandIdentityMismatch { source, actual } => write!(
                formatter,
                "identity bootstrap source {source:?} supplied a frame claiming {actual:?}"
            ),
            Self::ControllerIdentityMismatch(source) => write!(
                formatter,
                "identity bootstrap {source:?} controller does not match the template"
            ),
            Self::BootIdentityChanged(source) => write!(
                formatter,
                "identity bootstrap {source:?} controller changed boot identity"
            ),
            Self::SourceDisconnected(source) => write!(
                formatter,
                "identity bootstrap {source:?} controller disconnected before lock"
            ),
            Self::BufferCapacityExceeded => {
                formatter.write_str("identity bootstrap evidence exceeded its bounded buffer")
            }
            Self::Timeout => formatter.write_str("identity bootstrap timed out"),
            Self::Incomplete => formatter.write_str("identity bootstrap is not complete"),
        }
    }
}

impl Error for IdentityBootstrapError {}

/// Bounded pre-capture state that learns boot IDs from exact controller hashes.
pub struct DeviceWireIdentityBootstrap {
    config: IdentityBootstrapConfig,
    sources: BTreeMap<Hand, BootstrapSource>,
    events: Vec<DeviceTransportEvent>,
    buffered_bytes: usize,
    first_time_ns: Option<u64>,
    maximum_time_ns: Option<u64>,
}

impl DeviceWireIdentityBootstrap {
    /// Create a one- or two-controller bootstrap from a manifest template.
    pub fn new(
        expected: impl IntoIterator<Item = (Hand, [u8; 32])>,
        config: IdentityBootstrapConfig,
    ) -> Result<Self, IdentityBootstrapError> {
        if config.timeout_ns == 0
            || config.max_buffered_bytes == 0
            || config.max_buffered_events == 0
        {
            return Err(IdentityBootstrapError::InvalidConfiguration);
        }
        let mut sources = BTreeMap::new();
        for (hand, controller_id_sha256) in expected {
            if controller_id_sha256 == [0; 32]
                || sources
                    .insert(
                        hand,
                        BootstrapSource {
                            expected_controller_id_sha256: controller_id_sha256,
                            decoder: TransportDecoder::new(),
                            identity: None,
                        },
                    )
                    .is_some()
            {
                return Err(IdentityBootstrapError::InvalidTopology);
            }
        }
        if !(1..=2).contains(&sources.len()) {
            return Err(IdentityBootstrapError::InvalidTopology);
        }
        Ok(Self {
            config,
            sources,
            events: Vec::new(),
            buffered_bytes: 0,
            first_time_ns: None,
            maximum_time_ns: None,
        })
    }

    /// Consume bytes or host-time ticks until every physical boot is proven.
    pub fn handle(&mut self, event: DeviceTransportEvent) -> Result<(), IdentityBootstrapError> {
        let event_time_ns = match &event {
            DeviceTransportEvent::Bytes {
                receipt_time_ns, ..
            }
            | DeviceTransportEvent::Disconnected {
                receipt_time_ns, ..
            } => *receipt_time_ns,
            DeviceTransportEvent::Tick { host_time_ns } => *host_time_ns,
        };
        self.observe_time(event_time_ns)?;

        match event {
            DeviceTransportEvent::Bytes {
                source,
                receipt_time_ns,
                bytes,
            } => self.push_bytes(source, receipt_time_ns, bytes),
            DeviceTransportEvent::Disconnected { source, .. } => {
                if !self.sources.contains_key(&source) {
                    return Err(IdentityBootstrapError::UndeclaredSource(source));
                }
                Err(IdentityBootstrapError::SourceDisconnected(source))
            }
            DeviceTransportEvent::Tick { .. } => Ok(()),
        }
    }

    /// True only after each declared source supplied a matching valid frame.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.sources
            .values()
            .all(|source| source.identity.is_some())
    }

    /// Consume completed state and return canonical identities plus raw events.
    pub fn finish(self) -> Result<BootstrappedDeviceIdentity, IdentityBootstrapError> {
        if !self.is_complete() {
            return Err(IdentityBootstrapError::Incomplete);
        }
        let identities = self
            .sources
            .values()
            .map(|source| source.identity.expect("complete identity exists"))
            .collect();
        Ok(BootstrappedDeviceIdentity {
            identities,
            events: self.events,
        })
    }

    fn push_bytes(
        &mut self,
        source: Hand,
        receipt_time_ns: u64,
        bytes: Vec<u8>,
    ) -> Result<(), IdentityBootstrapError> {
        let Some(state) = self.sources.get(&source) else {
            return Err(IdentityBootstrapError::UndeclaredSource(source));
        };
        if self.events.len() >= self.config.max_buffered_events
            || self.buffered_bytes.saturating_add(bytes.len()) > self.config.max_buffered_bytes
        {
            return Err(IdentityBootstrapError::BufferCapacityExceeded);
        }

        let mut decoder = state.decoder.clone();
        let candidates = decoder.push(&bytes);
        let expected_controller_id_sha256 = state.expected_controller_id_sha256;
        let mut identity = state.identity;
        for candidate in candidates {
            let TransportCandidate::Decoded(payload) = candidate else {
                continue;
            };
            let Ok(frame) = decode_frame(&payload) else {
                continue;
            };
            if frame.hand != source {
                return Err(IdentityBootstrapError::HandIdentityMismatch {
                    source,
                    actual: frame.hand,
                });
            }
            if frame.controller_id_sha256 != expected_controller_id_sha256 {
                return Err(IdentityBootstrapError::ControllerIdentityMismatch(source));
            }
            if identity.is_some_and(|bound| bound.boot_id != frame.boot_id) {
                return Err(IdentityBootstrapError::BootIdentityChanged(source));
            }
            identity = Some(frame.identity());
        }

        let state = self
            .sources
            .get_mut(&source)
            .expect("declared source remains available");
        state.decoder = decoder;
        state.identity = identity;
        self.buffered_bytes += bytes.len();
        self.events.push(DeviceTransportEvent::Bytes {
            source,
            receipt_time_ns,
            bytes,
        });
        Ok(())
    }

    fn observe_time(&mut self, time_ns: u64) -> Result<(), IdentityBootstrapError> {
        self.first_time_ns = Some(
            self.first_time_ns
                .map_or(time_ns, |first| first.min(time_ns)),
        );
        let maximum = self
            .maximum_time_ns
            .map_or(time_ns, |previous| previous.max(time_ns));
        self.maximum_time_ns = Some(maximum);
        if maximum.saturating_sub(
            self.first_time_ns
                .expect("observing time establishes a first time"),
        ) > self.config.timeout_ns
        {
            return Err(IdentityBootstrapError::Timeout);
        }
        Ok(())
    }
}

/// Bounds and timing policy for one device transport merger.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeviceTransportConfig {
    /// Maximum cross-thread delivery reorder tolerated before a receipt is late.
    pub merge_lateness_ns: u64,
    /// Connected-source inactivity interval that produces one silence fault.
    pub source_silence_timeout_ns: u64,
    /// Maximum candidates retained after every watermark flush.
    pub max_pending_candidates: usize,
}

impl Default for DeviceTransportConfig {
    fn default() -> Self {
        Self {
            merge_lateness_ns: DEFAULT_MERGE_LATENESS_NS,
            source_silence_timeout_ns: DEFAULT_SOURCE_SILENCE_TIMEOUT_NS,
            max_pending_candidates: DEFAULT_MAX_PENDING_CANDIDATES,
        }
    }
}

/// Owned messages suitable for a bounded multi-producer channel.
///
/// Reader threads timestamp chunks before sending them. This module owns no
/// file descriptors, threads, clocks, or retry policy.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DeviceTransportEvent {
    /// Bytes read from exactly one declared controller stream.
    Bytes {
        source: Hand,
        receipt_time_ns: u64,
        bytes: Vec<u8>,
    },
    /// EOF or an I/O failure ended exactly one controller stream.
    Disconnected { source: Hand, receipt_time_ns: u64 },
    /// Advance host time when no bytes arrive.
    Tick { host_time_ns: u64 },
}

/// Whether a COBS-decoded candidate proves the declared source identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CandidateIdentity {
    /// A valid sensor frame matches hand, controller, and boot identity.
    Matched,
    /// No valid sensor frame exists, so identity cannot be read.
    Unavailable,
    /// A valid sensor frame differs from the declared physical source.
    Mismatch {
        expected: FrameIdentity,
        actual: FrameIdentity,
    },
}

/// One candidate released from the bounded merger.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourcedTransportCandidate {
    /// Declared controller stream that supplied the bytes.
    pub source: Hand,
    /// Timestamp of the chunk that completed or terminated this candidate.
    pub receipt_time_ns: u64,
    /// Zero-based candidate order within this source.
    pub source_ordinal: u64,
    /// Source validation performed before release.
    pub identity: CandidateIdentity,
    /// Exact bounded transport evidence recovered by the per-source decoder.
    pub candidate: TransportCandidate,
    /// The exact decode result already obtained during identity admission.
    pub(crate) frame: Option<Result<SensorFrame, FrameError>>,
}

/// Source-aware faults that callers should record and act on immediately.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeviceTransportFault {
    /// A valid frame differs from its physical source stream identity.
    IdentityMismatch {
        source: Hand,
        expected: FrameIdentity,
        actual: FrameIdentity,
        receipt_time_ns: u64,
        source_ordinal: u64,
    },
    /// A connected source exceeded the configured inactivity interval.
    SourceSilent {
        source: Hand,
        detected_time_ns: u64,
        last_receipt_time_ns: Option<u64>,
    },
    /// EOF or an I/O failure ended a source stream.
    SourceDisconnected { source: Hand, receipt_time_ns: u64 },
}

/// Results produced while handling one input event.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DeviceTransportUpdate {
    /// Candidates released in monotonic receipt order.
    pub candidates: Vec<SourcedTransportCandidate>,
    /// Source faults detected by this input event.
    pub faults: Vec<DeviceTransportFault>,
}

/// Invalid source topology rejected before reader threads start.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeviceTransportConfigurationError {
    NoSources,
    DuplicateSource(Hand),
    MissingControllerIdentity(Hand),
    MissingBootIdentity(Hand),
    ZeroPendingCapacity,
}

impl fmt::Display for DeviceTransportConfigurationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoSources => formatter.write_str("device transport has no declared sources"),
            Self::DuplicateSource(source) => {
                write!(
                    formatter,
                    "device transport source {source:?} is duplicated"
                )
            }
            Self::MissingControllerIdentity(source) => write!(
                formatter,
                "device transport source {source:?} has an all-zero controller identity"
            ),
            Self::MissingBootIdentity(source) => write!(
                formatter,
                "device transport source {source:?} has an all-zero boot identity"
            ),
            Self::ZeroPendingCapacity => {
                formatter.write_str("device transport pending capacity must be nonzero")
            }
        }
    }
}

impl Error for DeviceTransportConfigurationError {}

/// Input or ordering failure after construction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeviceTransportError {
    UndeclaredSource(Hand),
    SourceDisconnected(Hand),
    ReceiptRegression {
        source: Hand,
        previous: u64,
        actual: u64,
    },
    MergeLatenessExceeded {
        source: Hand,
        receipt_time_ns: u64,
        watermark_ns: u64,
    },
    HostClockRegression {
        previous: u64,
        actual: u64,
    },
    PendingCapacityExceeded {
        maximum: usize,
        required: usize,
    },
}

impl fmt::Display for DeviceTransportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UndeclaredSource(source) => {
                write!(
                    formatter,
                    "device transport source {source:?} is undeclared"
                )
            }
            Self::SourceDisconnected(source) => {
                write!(
                    formatter,
                    "device transport source {source:?} is disconnected"
                )
            }
            Self::ReceiptRegression {
                source,
                previous,
                actual,
            } => write!(
                formatter,
                "device transport source {source:?} receipt regressed from {previous} ns to {actual} ns"
            ),
            Self::MergeLatenessExceeded {
                source,
                receipt_time_ns,
                watermark_ns,
            } => write!(
                formatter,
                "device transport source {source:?} receipt {receipt_time_ns} ns is not later than watermark {watermark_ns} ns"
            ),
            Self::HostClockRegression { previous, actual } => write!(
                formatter,
                "device transport host clock regressed from {previous} ns to {actual} ns"
            ),
            Self::PendingCapacityExceeded { maximum, required } => write!(
                formatter,
                "device transport needs {required} pending candidates; maximum is {maximum}"
            ),
        }
    }
}

impl Error for DeviceTransportError {}

#[derive(Clone, Debug)]
struct SourceState {
    expected_identity: FrameIdentity,
    decoder: TransportDecoder,
    last_receipt_time_ns: Option<u64>,
    next_ordinal: u64,
    connected: bool,
    silence_reported: bool,
}

impl SourceState {
    const fn new(expected_identity: FrameIdentity) -> Self {
        Self {
            expected_identity,
            decoder: TransportDecoder::new(),
            last_receipt_time_ns: None,
            next_ordinal: 0,
            connected: true,
            silence_reported: false,
        }
    }
}

/// One bounded, source-aware merger around a decoder per declared hand.
pub struct DeviceTransport {
    config: DeviceTransportConfig,
    sources: BTreeMap<Hand, SourceState>,
    pending: Vec<SourcedTransportCandidate>,
    first_observed_time_ns: Option<u64>,
    maximum_observed_time_ns: Option<u64>,
    watermark_ns: Option<u64>,
    last_released_receipt_time_ns: Option<u64>,
}

impl DeviceTransport {
    /// Create independent stream state for every declared source.
    ///
    /// # Errors
    ///
    /// Rejects an empty topology, duplicate hands, or a zero pending limit.
    pub fn new(
        sources: impl IntoIterator<Item = FrameIdentity>,
        config: DeviceTransportConfig,
    ) -> Result<Self, DeviceTransportConfigurationError> {
        if config.max_pending_candidates == 0 {
            return Err(DeviceTransportConfigurationError::ZeroPendingCapacity);
        }

        let mut source_states = BTreeMap::new();
        for identity in sources {
            if identity.controller_id_sha256 == [0; 32] {
                return Err(
                    DeviceTransportConfigurationError::MissingControllerIdentity(identity.hand),
                );
            }
            if identity.boot_id == [0; 16] {
                return Err(DeviceTransportConfigurationError::MissingBootIdentity(
                    identity.hand,
                ));
            }
            if source_states
                .insert(identity.hand, SourceState::new(identity))
                .is_some()
            {
                return Err(DeviceTransportConfigurationError::DuplicateSource(
                    identity.hand,
                ));
            }
        }
        if source_states.is_empty() {
            return Err(DeviceTransportConfigurationError::NoSources);
        }

        Ok(Self {
            config,
            sources: source_states,
            pending: Vec::new(),
            first_observed_time_ns: None,
            maximum_observed_time_ns: None,
            watermark_ns: None,
            last_released_receipt_time_ns: None,
        })
    }

    /// Process one borrowed event and return released candidates and faults.
    ///
    /// # Errors
    ///
    /// Rejects undeclared or disconnected sources, per-source receipt
    /// regression, arrivals behind the committed watermark, host-clock
    /// regression, and candidate backlogs above the configured hard limit.
    /// The caller retains a rejected event for terminal evidence finalization;
    /// none of its bytes or decoder state are committed on an admission error.
    pub fn handle(
        &mut self,
        event: &DeviceTransportEvent,
    ) -> Result<DeviceTransportUpdate, DeviceTransportError> {
        self.handle_observed(event, |_| {})
    }

    /// Observe admitted completed candidates exactly once before the receipt
    /// merger can defer them. The observer never alters or discards raw facts.
    pub(crate) fn handle_observed(
        &mut self,
        event: &DeviceTransportEvent,
        mut observe: impl FnMut(&SourcedTransportCandidate),
    ) -> Result<DeviceTransportUpdate, DeviceTransportError> {
        match event {
            DeviceTransportEvent::Bytes {
                source,
                receipt_time_ns,
                bytes,
            } => self.push_bytes(*source, *receipt_time_ns, bytes, &mut observe),
            DeviceTransportEvent::Disconnected {
                source,
                receipt_time_ns,
            } => self.disconnect(*source, *receipt_time_ns, &mut observe),
            DeviceTransportEvent::Tick { host_time_ns } => self.tick(*host_time_ns),
        }
    }

    /// Retain admitted input after the caller has terminally revoked output.
    ///
    /// This drains merge-held candidates before consuming the event through the
    /// same per-source decoder. Receipt order, watermark and operational capacity
    /// no longer admit work: timestamps remain unmodified evidence, and candidate
    /// order follows each source's byte stream, even when receipts regress.
    /// These updates must never be delivered to a live runtime.
    ///
    /// # Errors
    ///
    /// Rejects an undeclared source without consuming any retained candidates.
    pub fn retain_evidence(
        &mut self,
        event: DeviceTransportEvent,
    ) -> Result<DeviceTransportUpdate, DeviceTransportError> {
        let source = match &event {
            DeviceTransportEvent::Bytes { source, .. }
            | DeviceTransportEvent::Disconnected { source, .. } => Some(*source),
            DeviceTransportEvent::Tick { .. } => None,
        };
        if let Some(source) = source
            && !self.sources.contains_key(&source)
        {
            return Err(DeviceTransportError::UndeclaredSource(source));
        }
        let mut retained = std::mem::take(&mut self.pending);
        retained.sort_by_key(|candidate| (candidate.source, candidate.source_ordinal));
        match event {
            DeviceTransportEvent::Bytes {
                source,
                receipt_time_ns,
                bytes,
            } => {
                let state = self
                    .sources
                    .get_mut(&source)
                    .expect("declared evidence source");
                let candidates = state.decoder.push(&bytes);
                let count = candidates.len();
                retained.extend(sourced_candidates(
                    source,
                    state.expected_identity,
                    receipt_time_ns,
                    state.next_ordinal,
                    candidates,
                    &mut |_| {},
                ));
                state.next_ordinal = state
                    .next_ordinal
                    .saturating_add(u64::try_from(count).unwrap_or(u64::MAX));
                state.last_receipt_time_ns = Some(receipt_time_ns);
            }
            DeviceTransportEvent::Disconnected {
                source,
                receipt_time_ns,
            } => {
                let state = self
                    .sources
                    .get_mut(&source)
                    .expect("declared evidence source");
                let candidates: Vec<_> = state.decoder.finish().into_iter().collect();
                let count = candidates.len();
                retained.extend(sourced_candidates(
                    source,
                    state.expected_identity,
                    state.last_receipt_time_ns.unwrap_or(receipt_time_ns),
                    state.next_ordinal,
                    candidates,
                    &mut |_| {},
                ));
                state.next_ordinal = state
                    .next_ordinal
                    .saturating_add(u64::try_from(count).unwrap_or(u64::MAX));
                state.connected = false;
            }
            DeviceTransportEvent::Tick { .. } => {}
        }
        Ok(DeviceTransportUpdate {
            candidates: retained,
            faults: Vec::new(),
        })
    }

    /// Return the current retained-candidate count for monitoring and tests.
    #[must_use]
    pub fn pending_candidates(&self) -> usize {
        self.pending.len()
    }

    /// Return the latest committed merge watermark, if lateness has elapsed.
    #[must_use]
    pub(crate) const fn watermark_ns(&self) -> Option<u64> {
        self.watermark_ns
    }

    fn push_bytes(
        &mut self,
        source: Hand,
        receipt_time_ns: u64,
        bytes: &[u8],
        observe: &mut impl FnMut(&SourcedTransportCandidate),
    ) -> Result<DeviceTransportUpdate, DeviceTransportError> {
        self.validate_receipt(source, receipt_time_ns)?;

        // Decode against a clone first. A capacity error leaves both the COBS
        // state and source ordinal unchanged, so callers never get a partial
        // acceptance of one reader-thread message.
        let state = self
            .sources
            .get(&source)
            .expect("validated declared source exists");
        let mut decoder = state.decoder.clone();
        let candidates = decoder.push(bytes);
        let prospective_watermark = self.prospective_watermark(receipt_time_ns);
        self.validate_pending_capacity(candidates.len(), receipt_time_ns, prospective_watermark)?;
        let sourced = sourced_candidates(
            source,
            state.expected_identity,
            receipt_time_ns,
            state.next_ordinal,
            candidates,
            observe,
        );

        let state = self
            .sources
            .get_mut(&source)
            .expect("validated declared source exists");
        state.decoder = decoder;
        state.last_receipt_time_ns = Some(receipt_time_ns);
        state.next_ordinal = state
            .next_ordinal
            .saturating_add(u64::try_from(sourced.len()).unwrap_or(u64::MAX));
        state.silence_reported = false;

        let mut faults = identity_faults(&sourced);
        self.observe_time(receipt_time_ns);
        self.pending.extend(sourced);
        faults.extend(
            self.detect_silence(
                self.maximum_observed_time_ns
                    .expect("receipt time was just observed"),
            ),
        );
        Ok(DeviceTransportUpdate {
            candidates: self.flush_ready(),
            faults,
        })
    }

    fn disconnect(
        &mut self,
        source: Hand,
        receipt_time_ns: u64,
        observe: &mut impl FnMut(&SourcedTransportCandidate),
    ) -> Result<DeviceTransportUpdate, DeviceTransportError> {
        self.validate_receipt(source, receipt_time_ns)?;

        let state = self
            .sources
            .get(&source)
            .expect("validated declared source exists");
        let mut decoder = state.decoder.clone();
        let candidates: Vec<_> = decoder.finish().into_iter().collect();
        let prospective_watermark = self.prospective_watermark(receipt_time_ns);
        self.validate_pending_capacity(candidates.len(), receipt_time_ns, prospective_watermark)?;
        let sourced = sourced_candidates(
            source,
            state.expected_identity,
            receipt_time_ns,
            state.next_ordinal,
            candidates,
            observe,
        );

        let state = self
            .sources
            .get_mut(&source)
            .expect("validated declared source exists");
        state.decoder = decoder;
        state.last_receipt_time_ns = Some(receipt_time_ns);
        state.next_ordinal = state
            .next_ordinal
            .saturating_add(u64::try_from(sourced.len()).unwrap_or(u64::MAX));
        state.connected = false;

        self.observe_time(receipt_time_ns);
        self.pending.extend(sourced);
        let mut faults = vec![DeviceTransportFault::SourceDisconnected {
            source,
            receipt_time_ns,
        }];
        faults.extend(
            self.detect_silence(
                self.maximum_observed_time_ns
                    .expect("receipt time was just observed"),
            ),
        );
        let candidates = if self.sources.values().all(|state| !state.connected) {
            self.flush_all()
        } else {
            self.flush_ready()
        };
        Ok(DeviceTransportUpdate { candidates, faults })
    }

    fn tick(&mut self, host_time_ns: u64) -> Result<DeviceTransportUpdate, DeviceTransportError> {
        if self
            .maximum_observed_time_ns
            .is_some_and(|previous| host_time_ns < previous)
        {
            return Err(DeviceTransportError::HostClockRegression {
                previous: self
                    .maximum_observed_time_ns
                    .expect("regression requires a previous time"),
                actual: host_time_ns,
            });
        }
        self.observe_time(host_time_ns);
        Ok(DeviceTransportUpdate {
            candidates: self.flush_ready(),
            faults: self.detect_silence(host_time_ns),
        })
    }

    fn validate_receipt(
        &self,
        source: Hand,
        receipt_time_ns: u64,
    ) -> Result<(), DeviceTransportError> {
        let Some(state) = self.sources.get(&source) else {
            return Err(DeviceTransportError::UndeclaredSource(source));
        };
        if !state.connected {
            return Err(DeviceTransportError::SourceDisconnected(source));
        }
        if let Some(previous) = state.last_receipt_time_ns
            && receipt_time_ns < previous
        {
            return Err(DeviceTransportError::ReceiptRegression {
                source,
                previous,
                actual: receipt_time_ns,
            });
        }
        if let Some(watermark_ns) = self.watermark_ns
            && receipt_time_ns <= watermark_ns
        {
            return Err(DeviceTransportError::MergeLatenessExceeded {
                source,
                receipt_time_ns,
                watermark_ns,
            });
        }
        Ok(())
    }

    fn validate_pending_capacity(
        &self,
        incoming_count: usize,
        receipt_time_ns: u64,
        prospective_watermark: Option<u64>,
    ) -> Result<(), DeviceTransportError> {
        let remains_pending = |candidate: &&SourcedTransportCandidate| {
            prospective_watermark.is_none_or(|watermark| candidate.receipt_time_ns > watermark)
        };
        let incoming_pending =
            if prospective_watermark.is_none_or(|watermark| receipt_time_ns > watermark) {
                incoming_count
            } else {
                0
            };
        let required = self.pending.iter().filter(remains_pending).count() + incoming_pending;
        if required > self.config.max_pending_candidates {
            return Err(DeviceTransportError::PendingCapacityExceeded {
                maximum: self.config.max_pending_candidates,
                required,
            });
        }
        Ok(())
    }

    fn prospective_watermark(&self, observed_time_ns: u64) -> Option<u64> {
        self.maximum_observed_time_ns
            .map_or(observed_time_ns, |previous| previous.max(observed_time_ns))
            .checked_sub(self.config.merge_lateness_ns)
    }

    fn observe_time(&mut self, observed_time_ns: u64) {
        self.first_observed_time_ns = Some(
            self.first_observed_time_ns
                .map_or(observed_time_ns, |first| first.min(observed_time_ns)),
        );
        let maximum = self
            .maximum_observed_time_ns
            .map_or(observed_time_ns, |previous| previous.max(observed_time_ns));
        self.maximum_observed_time_ns = Some(maximum);
        self.watermark_ns = maximum.checked_sub(self.config.merge_lateness_ns);
    }

    fn detect_silence(&mut self, host_time_ns: u64) -> Vec<DeviceTransportFault> {
        let Some(first_observed_time_ns) = self.first_observed_time_ns else {
            return Vec::new();
        };
        let mut faults = Vec::new();
        for (source, state) in &mut self.sources {
            if !state.connected || state.silence_reported {
                continue;
            }
            let reference_time_ns = state.last_receipt_time_ns.unwrap_or(first_observed_time_ns);
            if host_time_ns.saturating_sub(reference_time_ns)
                > self.config.source_silence_timeout_ns
            {
                state.silence_reported = true;
                faults.push(DeviceTransportFault::SourceSilent {
                    source: *source,
                    detected_time_ns: host_time_ns,
                    last_receipt_time_ns: state.last_receipt_time_ns,
                });
            }
        }
        faults
    }

    fn flush_ready(&mut self) -> Vec<SourcedTransportCandidate> {
        let Some(watermark_ns) = self.watermark_ns else {
            return Vec::new();
        };
        self.flush(Some(watermark_ns))
    }

    fn flush_all(&mut self) -> Vec<SourcedTransportCandidate> {
        self.flush(None)
    }

    fn flush(&mut self, watermark_ns: Option<u64>) -> Vec<SourcedTransportCandidate> {
        let pending = std::mem::take(&mut self.pending);
        let (mut released, retained) = pending.into_iter().partition(|candidate| {
            watermark_ns.is_none_or(|watermark| candidate.receipt_time_ns <= watermark)
        });
        self.pending = retained;
        released.sort_by_key(|candidate: &SourcedTransportCandidate| {
            (
                candidate.receipt_time_ns,
                candidate.source,
                candidate.source_ordinal,
            )
        });
        for candidate in &released {
            debug_assert!(
                self.last_released_receipt_time_ns
                    .is_none_or(|last| candidate.receipt_time_ns >= last),
                "device transport released a receipt-time regression"
            );
            self.last_released_receipt_time_ns = Some(candidate.receipt_time_ns);
        }
        released
    }
}

fn sourced_candidates(
    source: Hand,
    expected_identity: FrameIdentity,
    receipt_time_ns: u64,
    first_ordinal: u64,
    candidates: Vec<TransportCandidate>,
    observe: &mut impl FnMut(&SourcedTransportCandidate),
) -> Vec<SourcedTransportCandidate> {
    candidates
        .into_iter()
        .enumerate()
        .map(|(offset, candidate)| {
            let source_ordinal =
                first_ordinal.saturating_add(u64::try_from(offset).unwrap_or(u64::MAX));
            let (identity, frame) = candidate_identity(expected_identity, &candidate);
            let sourced = SourcedTransportCandidate {
                source,
                receipt_time_ns,
                source_ordinal,
                identity,
                candidate,
                frame,
            };
            observe(&sourced);
            sourced
        })
        .collect()
}

fn candidate_identity(
    expected_identity: FrameIdentity,
    candidate: &TransportCandidate,
) -> (CandidateIdentity, Option<Result<SensorFrame, FrameError>>) {
    let TransportCandidate::Decoded(payload) = candidate else {
        return (CandidateIdentity::Unavailable, None);
    };
    let frame = decode_frame(payload);
    let identity = match &frame {
        Ok(frame) if frame.identity() == expected_identity => CandidateIdentity::Matched,
        Ok(frame) => CandidateIdentity::Mismatch {
            expected: expected_identity,
            actual: frame.identity(),
        },
        Err(_) => CandidateIdentity::Unavailable,
    };
    (identity, Some(frame))
}

fn identity_faults(candidates: &[SourcedTransportCandidate]) -> Vec<DeviceTransportFault> {
    candidates
        .iter()
        .filter_map(|candidate| {
            let CandidateIdentity::Mismatch { expected, actual } = candidate.identity else {
                return None;
            };
            Some(DeviceTransportFault::IdentityMismatch {
                source: candidate.source,
                expected,
                actual,
                receipt_time_ns: candidate.receipt_time_ns,
                source_ordinal: candidate.source_ordinal,
            })
        })
        .collect()
}
