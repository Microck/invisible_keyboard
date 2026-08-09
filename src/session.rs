//! Append-only session recording, deterministic replay, and stream accounting.

use std::{
    error::Error,
    fmt,
    io::{self, Read, Write},
    sync::Arc,
};

use sha2::{Digest, Sha256};

use crate::{
    calibration::SETUP_TIME_CAP_SECONDS,
    layout::Hand,
    protocol::{
        ALL_SENSORS_VALID, FLAG_ACQUISITION_OVERRUN, FLAG_CLOCK_UNSYNCED, FrameError,
        FrameIdentity, QUIESCE_RECEIPT_LEN, QUIESCE_REQUEST_LEN, QuiesceReceipt,
        QuiesceRecordError, QuiesceRequest, SENSOR_COUNT, SensorFrame, SensorReading, checksum,
        checksum_parts, decode_frame, decode_quiesce_receipt, decode_quiesce_request, encode_frame,
        encode_quiesce_receipt, encode_quiesce_request,
    },
    transport::{MAX_ENCODED_FRAME_LEN, TransportCandidate},
};

/// Exact byte length of the current session header.
pub const HEADER_LEN: usize = 64;

/// Exact byte length of the mandatory session completion footer.
pub const FOOTER_LEN: usize = 16;

/// Fixed bytes before each record payload, including the record magic.
pub const RECORD_PREFIX_LEN: usize = 28;

/// Maximum captured transport payload. The canonical frame is only 160 bytes;
/// this ceiling retains malformed transport evidence without trusting an
/// unbounded length field.
pub const MAX_CAPTURE_PAYLOAD: usize = 4_096;

/// Canonical aggregate frame rate emitted by each controller.
pub const ACQUISITION_RECORDS_PER_SECOND_PER_HAND: u64 = 200;

/// Largest persisted record one canonical controller transport candidate can
/// produce: record prefix, encoded candidate, and record checksum.
pub const MAX_ACQUISITION_RECORD_BYTES: u64 =
    RECORD_PREFIX_LEN as u64 + MAX_ENCODED_FRAME_LEN as u64 + std::mem::size_of::<u32>() as u64;

/// Maximum records admitted by a finite calibration or replay bundle.
pub const MAX_SETUP_SESSION_RECORDS: u64 =
    SETUP_TIME_CAP_SECONDS as u64 * 2 * ACQUISITION_RECORDS_PER_SECOND_PER_HAND;

/// Maximum finite calibration/replay session size.
///
/// The bound covers the 1,200-second setup cap, both 200 Hz controllers, the
/// largest transport candidate each acquisition tick can retain, and the
/// fixed session header/footer. Live recording does not use this bound.
pub const MAX_SETUP_SESSION_BYTES: u64 = HEADER_LEN as u64
    + MAX_SETUP_SESSION_RECORDS * MAX_ACQUISITION_RECORD_BYTES
    + FOOTER_LEN as u64;

/// Fixed duration of the canonical Gate 0A acquisition.
pub const GATE0A_CAPTURE_TIME_SECONDS: u64 = 3_610;

/// Maximum sensor plus terminal-control records in the Gate 0A contract.
pub const MAX_GATE0A_SESSION_RECORDS: u64 =
    GATE0A_CAPTURE_TIME_SECONDS * 2 * ACQUISITION_RECORDS_PER_SECOND_PER_HAND + 2;

/// Maximum Gate 0A session size at two 200 Hz controllers, plus the exact
/// request and receipt records.
pub const MAX_GATE0A_SESSION_BYTES: u64 = HEADER_LEN as u64
    + (MAX_GATE0A_SESSION_RECORDS - 2) * MAX_ACQUISITION_RECORD_BYTES
    + (RECORD_PREFIX_LEN + QUIESCE_REQUEST_LEN + std::mem::size_of::<u32>()) as u64
    + (RECORD_PREFIX_LEN + QUIESCE_RECEIPT_LEN + std::mem::size_of::<u32>()) as u64
    + FOOTER_LEN as u64;

// The source-aware schema deliberately has distinct magic bytes and a new
// version. Reusing the v1 reserved byte would make a legacy record silently
// look as if it came from the left controller.
const SESSION_MAGIC: &[u8; 4] = b"IKS3";
const SESSION_VERSION: u8 = 3;
const RECORD_MAGIC: &[u8; 4] = b"REC3";
const FOOTER_MAGIC: &[u8; 4] = b"END3";
const HEADER_CHECKSUM_OFFSET: usize = HEADER_LEN - 4;
const FOOTER_CHECKSUM_OFFSET: usize = FOOTER_LEN - 4;
const RECORD_METADATA_LEN: usize = RECORD_PREFIX_LEN - RECORD_MAGIC.len();
const REFERENCE_SESSION_RECORDS: u32 = 64;
/// SHA-256 of the ASCII Core-28 manifest identity.
pub const CORE28_MANIFEST_SHA256: [u8; 32] = [
    0xac, 0x6b, 0x70, 0xb8, 0xaa, 0x9f, 0x9f, 0x16, 0x8b, 0x47, 0x83, 0x63, 0x2b, 0x71, 0x16, 0xf6,
    0x75, 0xa5, 0xdf, 0x01, 0x8d, 0x30, 0x1d, 0x6e, 0xc1, 0xa9, 0x42, 0xcc, 0xd5, 0x9b, 0x12, 0x1f,
];

/// Immutable identity and acquisition settings for one session file.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SessionHeader {
    /// Intended aggregate frame period for each hand.
    pub nominal_period_us: u32,
    /// SHA-256 of the external canonical manifest bytes.
    pub manifest_sha256: [u8; 32],
    /// Opaque session identifier generated before acquisition starts.
    pub session_id: [u8; 16],
}

/// Physical USB controller stream that supplied one captured candidate.
///
/// This is intentionally distinct from [`Hand`]. A decoded payload can claim
/// either hand, but its physical source is fixed by the host reader that
/// received the bytes.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[repr(u8)]
pub enum ControllerStream {
    Left = 0,
    Right = 1,
}

impl ControllerStream {
    /// Hand identity a well-formed payload from this stream must claim.
    #[must_use]
    pub const fn expected_hand(self) -> Hand {
        match self {
            Self::Left => Hand::Left,
            Self::Right => Hand::Right,
        }
    }
}

impl From<Hand> for ControllerStream {
    fn from(hand: Hand) -> Self {
        match hand {
            Hand::Left => Self::Left,
            Hand::Right => Self::Right,
        }
    }
}

impl TryFrom<u8> for ControllerStream {
    type Error = u8;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Left),
            1 => Ok(Self::Right),
            invalid => Err(invalid),
        }
    }
}

/// Persisted IKS3 record taxonomy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum SessionRecordKind {
    Decoded = 0,
    Malformed = 1,
    Oversized = 2,
    Unterminated = 3,
    QuiesceRequest = 4,
    QuiesceReceipt = 5,
}

impl TryFrom<u8> for SessionRecordKind {
    type Error = u8;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Decoded),
            1 => Ok(Self::Malformed),
            2 => Ok(Self::Oversized),
            3 => Ok(Self::Unterminated),
            4 => Ok(Self::QuiesceRequest),
            5 => Ok(Self::QuiesceReceipt),
            invalid => Err(invalid),
        }
    }
}

/// Per-hand facts derived from the recorded stream in receive order.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct HandStreamSummary {
    pub received: u64,
    pub missing: u64,
    pub duplicates: u64,
    pub reordered: u64,
    pub device_time_regressions: u64,
    pub host_time_regressions: u64,
    pub frames_with_invalid_sensors: u64,
    pub invalid_sensor_readings: u64,
    pub clock_unsynced: u64,
    pub acquisition_overruns: u64,
    pub first_sequence: Option<u32>,
    pub last_in_order_sequence: Option<u32>,
}

/// Complete facts derivable without modifying or repairing the raw session.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SessionSummary {
    pub total_records: u64,
    pub invalid_records: u64,
    pub decoded_candidates: u64,
    pub malformed_candidates: u64,
    pub oversized_candidates: u64,
    pub unterminated_candidates: u64,
    pub quiesce_requests: u64,
    pub quiesce_receipts: u64,
    /// Structurally valid frames that claim a hand other than their source.
    pub source_payload_hand_mismatches: u64,
    pub left: HandStreamSummary,
    pub right: HandStreamSummary,
}

/// Immutable facts produced by a complete streaming strict audit.
///
/// Unlike [`FinalizedSession`], this value does not retain the session bytes.
/// It is therefore suitable for an arbitrarily long live capture after its
/// writer has closed and synced the file.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SessionAudit {
    sha256: [u8; 32],
    byte_len: u64,
    header: SessionHeader,
    summary: SessionSummary,
}

/// Maximum host request-start to receipt-observation interval.
pub const GATE0A_HOST_RESPONSE_TIMEOUT_NS: u64 = 750_000_000;

/// Compact proof that one IKS3 stream has the required Gate 0A ending.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Gate0aTerminalBinding {
    pub source: ControllerStream,
    pub request: QuiesceRequest,
    pub receipt: QuiesceReceipt,
    pub final_frame: SensorFrame,
    pub request_record_index: u64,
    pub final_frame_record_index: u64,
    pub receipt_record_index: u64,
    pub request_host_time_ns: u64,
    pub final_frame_host_time_ns: u64,
    pub receipt_host_time_ns: u64,
    pub response_time_ns: u64,
}

/// One complete strict IKS3 audit plus its verified terminal binding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Gate0aSessionAudit {
    session: SessionAudit,
    terminal: Gate0aTerminalBinding,
}

impl Gate0aSessionAudit {
    #[must_use]
    pub const fn session(&self) -> &SessionAudit {
        &self.session
    }

    #[must_use]
    pub const fn terminal(&self) -> &Gate0aTerminalBinding {
        &self.terminal
    }
}

impl SessionAudit {
    /// Path-independent SHA-256 of every audited byte, including the footer.
    #[must_use]
    pub const fn sha256(&self) -> [u8; 32] {
        self.sha256
    }

    /// Exact number of bytes consumed through the validated footer and EOF.
    #[must_use]
    pub const fn byte_len(&self) -> u64 {
        self.byte_len
    }

    /// Audited immutable session header.
    #[must_use]
    pub const fn header(&self) -> &SessionHeader {
        &self.header
    }

    /// Audited immutable stream accounting.
    #[must_use]
    pub const fn summary(&self) -> &SessionSummary {
        &self.summary
    }
}

impl SessionSummary {
    fn stream_mut(&mut self, source: ControllerStream) -> &mut HandStreamSummary {
        match source {
            ControllerStream::Left => &mut self.left,
            ControllerStream::Right => &mut self.right,
        }
    }

    /// Return whether both sensor streams are populated, continuous,
    /// monotonic, complete, and free of controller-reported faults.
    #[must_use]
    pub fn is_clean_two_hand_sensor_stream(&self) -> bool {
        self.invalid_records == 0
            && self.quiesce_requests == 0
            && self.quiesce_receipts == 0
            && [self.left, self.right].iter().all(|hand| {
                hand.received != 0
                    && hand.missing == 0
                    && hand.duplicates == 0
                    && hand.reordered == 0
                    && hand.device_time_regressions == 0
                    && hand.host_time_regressions == 0
                    && hand.frames_with_invalid_sensors == 0
                    && hand.clock_unsynced == 0
                    && hand.acquisition_overruns == 0
            })
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct HandTracker {
    last_in_order_sequence: Option<u32>,
    previous_device_time_us: Option<u64>,
    previous_host_time_ns: Option<u64>,
}

impl HandTracker {
    fn observe(&mut self, frame: &SensorFrame, host_time_ns: u64, summary: &mut HandStreamSummary) {
        summary.received += 1;

        if summary.first_sequence.is_none() {
            summary.first_sequence = Some(frame.sequence);
        }

        match self.last_in_order_sequence {
            None => {
                self.last_in_order_sequence = Some(frame.sequence);
                summary.last_in_order_sequence = Some(frame.sequence);
            }
            Some(previous) if frame.sequence == previous => summary.duplicates += 1,
            Some(previous) => {
                let expected = previous.wrapping_add(1);
                let forward_delta = frame.sequence.wrapping_sub(expected);
                if forward_delta == 0 {
                    self.last_in_order_sequence = Some(frame.sequence);
                    summary.last_in_order_sequence = Some(frame.sequence);
                } else if forward_delta < (1_u32 << 31) {
                    summary.missing += u64::from(forward_delta);
                    self.last_in_order_sequence = Some(frame.sequence);
                    summary.last_in_order_sequence = Some(frame.sequence);
                } else {
                    summary.reordered += 1;
                }
            }
        }

        if self
            .previous_device_time_us
            .is_some_and(|previous| frame.device_time_us < previous)
        {
            summary.device_time_regressions += 1;
        }
        if self
            .previous_host_time_ns
            .is_some_and(|previous| host_time_ns < previous)
        {
            summary.host_time_regressions += 1;
        }
        self.previous_device_time_us = Some(frame.device_time_us);
        self.previous_host_time_ns = Some(host_time_ns);

        let invalid_mask = (!frame.valid_sensor_mask) & ALL_SENSORS_VALID;
        if invalid_mask != 0 {
            summary.frames_with_invalid_sensors += 1;
            summary.invalid_sensor_readings += u64::from(invalid_mask.count_ones());
        }
        if frame.flags & FLAG_CLOCK_UNSYNCED != 0 {
            summary.clock_unsynced += 1;
        }
        if frame.flags & FLAG_ACQUISITION_OVERRUN != 0 {
            summary.acquisition_overruns += 1;
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct StreamAccounting {
    summary: SessionSummary,
    left: HandTracker,
    right: HandTracker,
}

/// The one canonical sensor-frame decode performed while accounting.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CandidateDecode {
    NotDecoded,
    Invalid(FrameError),
    Decoded(SensorFrame),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CandidateObservation {
    decode: CandidateDecode,
    claimed_hand_mismatch: Option<Hand>,
    invalid_quiesce: Option<QuiesceRecordError>,
}

impl StreamAccounting {
    fn observe_frame(
        &mut self,
        source: ControllerStream,
        frame: SensorFrame,
        host_time_ns: u64,
    ) -> CandidateObservation {
        self.summary.total_records += 1;
        self.summary.decoded_candidates += 1;
        self.observe_decoded_frame(source, frame, host_time_ns)
    }

    fn observe_decoded_frame(
        &mut self,
        source: ControllerStream,
        frame: SensorFrame,
        host_time_ns: u64,
    ) -> CandidateObservation {
        if frame.hand != source.expected_hand() {
            self.summary.invalid_records += 1;
            self.summary.source_payload_hand_mismatches += 1;
            return CandidateObservation {
                decode: CandidateDecode::Decoded(frame),
                claimed_hand_mismatch: Some(frame.hand),
                invalid_quiesce: None,
            };
        }

        let stream_summary = self.summary.stream_mut(source);
        match source {
            ControllerStream::Left => self.left.observe(&frame, host_time_ns, stream_summary),
            ControllerStream::Right => self.right.observe(&frame, host_time_ns, stream_summary),
        }
        CandidateObservation {
            decode: CandidateDecode::Decoded(frame),
            claimed_hand_mismatch: None,
            invalid_quiesce: None,
        }
    }

    /// Account for one fully written or fully parsed record.
    ///
    /// The observation retains the canonical decode and any source/payload
    /// mismatch. Recording preserves a mismatch and keeps going; strict audit
    /// turns the same fact into an error.
    fn observe_candidate(
        &mut self,
        source: ControllerStream,
        kind: SessionRecordKind,
        payload: &[u8],
        host_time_ns: u64,
    ) -> CandidateObservation {
        self.summary.total_records += 1;
        match kind {
            SessionRecordKind::Decoded => self.summary.decoded_candidates += 1,
            SessionRecordKind::Malformed => self.summary.malformed_candidates += 1,
            SessionRecordKind::Oversized => self.summary.oversized_candidates += 1,
            SessionRecordKind::Unterminated => self.summary.unterminated_candidates += 1,
            SessionRecordKind::QuiesceRequest => self.summary.quiesce_requests += 1,
            SessionRecordKind::QuiesceReceipt => self.summary.quiesce_receipts += 1,
        }

        let (decode, claimed_hand, invalid_quiesce) = match kind {
            SessionRecordKind::Malformed
            | SessionRecordKind::Oversized
            | SessionRecordKind::Unterminated => {
                self.summary.invalid_records += 1;
                (CandidateDecode::NotDecoded, None, None)
            }
            SessionRecordKind::Decoded => match decode_frame(payload) {
                Ok(frame) => (CandidateDecode::Decoded(frame), Some(frame.hand), None),
                Err(error) => {
                    self.summary.invalid_records += 1;
                    (CandidateDecode::Invalid(error), None, None)
                }
            },
            SessionRecordKind::QuiesceRequest => match decode_quiesce_request(payload) {
                Ok(request) => (CandidateDecode::NotDecoded, Some(request.hand), None),
                Err(error) => {
                    self.summary.invalid_records += 1;
                    (CandidateDecode::NotDecoded, None, Some(error))
                }
            },
            SessionRecordKind::QuiesceReceipt => match decode_quiesce_receipt(payload) {
                Ok(receipt) => (CandidateDecode::NotDecoded, Some(receipt.hand), None),
                Err(error) => {
                    self.summary.invalid_records += 1;
                    (CandidateDecode::NotDecoded, None, Some(error))
                }
            },
        };

        if let CandidateDecode::Decoded(frame) = decode {
            return self.observe_decoded_frame(source, frame, host_time_ns);
        }

        if claimed_hand.is_some_and(|hand| hand != source.expected_hand()) {
            self.summary.invalid_records += 1;
            self.summary.source_payload_hand_mismatches += 1;
            return CandidateObservation {
                decode,
                claimed_hand_mismatch: claimed_hand,
                invalid_quiesce,
            };
        }

        CandidateObservation {
            decode,
            claimed_hand_mismatch: None,
            invalid_quiesce,
        }
    }
}

/// Append-only writer that preserves raw transport payloads before decoding.
pub struct SessionWriter<W> {
    writer: W,
    accounting: StreamAccounting,
}

impl<W: Write> SessionWriter<W> {
    /// Write a new session header and prepare to append records.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::Io`] if the destination cannot accept the full
    /// header. A partial write is an invalid session and must not be resumed.
    pub fn new(mut writer: W, header: SessionHeader) -> Result<Self, SessionError> {
        writer.write_all(&encode_header(header))?;
        Ok(Self {
            writer,
            accounting: StreamAccounting::default(),
        })
    }

    /// Append a canonical frame and account for its stream state.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the complete record cannot be appended.
    pub fn append_frame(
        &mut self,
        source: ControllerStream,
        frame: &SensorFrame,
        host_time_ns: u64,
    ) -> Result<(), SessionError> {
        let payload = encode_frame(frame)?;
        self.write_candidate(
            source,
            SessionRecordKind::Decoded,
            &payload,
            u64::try_from(payload.len()).expect("fixed frame length fits u64"),
            host_time_ns,
        )?;
        let _observation = self.accounting.observe_frame(source, *frame, host_time_ns);
        Ok(())
    }

    /// Append one exact outbound `IKQ2` request record.
    pub fn append_quiesce_request(
        &mut self,
        source: ControllerStream,
        request: &QuiesceRequest,
        host_time_ns: u64,
    ) -> Result<(), SessionError> {
        let payload = encode_quiesce_request(request)?;
        self.append_candidate(
            source,
            SessionRecordKind::QuiesceRequest,
            &payload,
            QUIESCE_REQUEST_LEN as u64,
            host_time_ns,
        )
    }

    /// Append one exact inbound `IKR2` receipt record.
    pub fn append_quiesce_receipt(
        &mut self,
        source: ControllerStream,
        receipt: &QuiesceReceipt,
        host_time_ns: u64,
    ) -> Result<(), SessionError> {
        let payload = encode_quiesce_receipt(receipt)?;
        self.append_candidate(
            source,
            SessionRecordKind::QuiesceReceipt,
            &payload,
            QUIESCE_RECEIPT_LEN as u64,
            host_time_ns,
        )
    }

    /// Append one candidate with its transport provenance and pre-truncation
    /// length.
    ///
    /// # Errors
    ///
    /// Returns before writing when the captured payload is too large or the
    /// original length contradicts the candidate kind. Returns an I/O error if
    /// the complete record cannot be appended.
    pub fn append_candidate(
        &mut self,
        source: ControllerStream,
        kind: SessionRecordKind,
        payload: &[u8],
        original_len: u64,
        host_time_ns: u64,
    ) -> Result<(), SessionError> {
        self.write_candidate(source, kind, payload, original_len, host_time_ns)?;
        let _observation = self
            .accounting
            .observe_candidate(source, kind, payload, host_time_ns);
        Ok(())
    }

    fn write_candidate(
        &mut self,
        source: ControllerStream,
        kind: SessionRecordKind,
        payload: &[u8],
        original_len: u64,
        host_time_ns: u64,
    ) -> Result<(), SessionError> {
        if payload.len() > MAX_CAPTURE_PAYLOAD {
            return Err(SessionError::PayloadTooLarge {
                actual: payload.len(),
            });
        }
        validate_candidate_length(kind, payload.len(), original_len, None)?;

        let payload_len =
            u32::try_from(payload.len()).expect("bounded capture payload length fits u32");
        let mut metadata = [0_u8; RECORD_METADATA_LEN];
        metadata[0] = kind as u8;
        metadata[1] = source as u8;
        metadata[4..8].copy_from_slice(&payload_len.to_le_bytes());
        metadata[8..16].copy_from_slice(&original_len.to_le_bytes());
        metadata[16..24].copy_from_slice(&host_time_ns.to_le_bytes());
        let record_checksum = checksum_parts(&[&metadata, payload]);

        self.writer.write_all(RECORD_MAGIC)?;
        self.writer.write_all(&metadata)?;
        self.writer.write_all(payload)?;
        self.writer.write_all(&record_checksum.to_le_bytes())?;
        Ok(())
    }

    /// Append one transport candidate without duplicating its persistence
    /// projection at each recorder boundary.
    ///
    /// # Errors
    ///
    /// Returns the same bounded-length or destination error as
    /// [`Self::append_candidate`].
    pub fn append_transport_candidate(
        &mut self,
        source: ControllerStream,
        candidate: &TransportCandidate,
        host_time_ns: u64,
    ) -> Result<(), SessionError> {
        let (kind, payload, original_len) = match candidate {
            TransportCandidate::Decoded(payload) => (
                SessionRecordKind::Decoded,
                payload.as_slice(),
                u64::try_from(payload.len()).unwrap_or(u64::MAX),
            ),
            TransportCandidate::Malformed { encoded } => (
                SessionRecordKind::Malformed,
                encoded.as_slice(),
                u64::try_from(encoded.len()).unwrap_or(u64::MAX),
            ),
            TransportCandidate::Oversized { prefix, actual_len } => (
                SessionRecordKind::Oversized,
                prefix.as_slice(),
                u64::try_from(*actual_len).unwrap_or(u64::MAX),
            ),
            TransportCandidate::Unterminated { encoded } => (
                SessionRecordKind::Unterminated,
                encoded.as_slice(),
                u64::try_from(encoded.len()).unwrap_or(u64::MAX),
            ),
        };
        self.append_candidate(source, kind, payload, original_len, host_time_ns)
    }

    /// Return a copy of accounting for records fully written so far.
    ///
    /// The snapshot is diagnostic only. It does not claim that the footer was
    /// written or that the complete byte stream passed strict audit.
    #[must_use]
    pub const fn accounting_snapshot(&self) -> SessionSummary {
        self.accounting.summary
    }

    /// Flush the header and complete records written so far without a footer.
    ///
    /// Only [`Self::finish`] writes `END3`. Dropping this writer after a
    /// failure therefore leaves an intentionally incomplete session.
    pub fn flush(&mut self) -> Result<(), SessionError> {
        self.writer.flush()?;
        Ok(())
    }

    /// Write the mandatory completion footer, flush the writer, and return it
    /// with the derived summary.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the underlying writer cannot flush.
    pub fn finish(mut self) -> Result<(W, SessionSummary), SessionError> {
        self.writer
            .write_all(&encode_footer(self.accounting.summary.total_records))?;
        self.writer.flush()?;
        Ok((self.writer, self.accounting.summary))
    }
}

impl SessionWriter<Vec<u8>> {
    /// Finish the byte container without admitting it as usable evidence.
    ///
    /// The completion footer is durable structure, not an evidence claim. This
    /// distinction lets incident handling retain a byte-exact, footer-closed
    /// stream even when strict provenance audit correctly rejects it.
    pub fn finish_closed(self) -> Result<ClosedSession, SessionError> {
        let (bytes, summary) = self.finish()?;
        Ok(ClosedSession { bytes, summary })
    }

    /// Finish an in-memory writer and audit every byte into immutable evidence.
    ///
    /// # Errors
    ///
    /// Returns the write error or the first strict audit failure. In
    /// particular, a decoded frame that claims the wrong hand cannot produce
    /// a [`FinalizedSession`].
    pub fn finish_finalized(self) -> Result<FinalizedSession, SessionError> {
        self.finish_closed()?.into_admitted()
    }
}

/// Immutable, footer-closed session bytes that have not passed strict audit.
///
/// This type is incident material only. APIs that fit clocks, derive evidence,
/// or validate a model continue to require [`FinalizedSession`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClosedSession {
    bytes: Vec<u8>,
    summary: SessionSummary,
}

impl ClosedSession {
    /// Exact bytes including the mandatory completion footer.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Writer-side accounting retained for incident diagnosis.
    #[must_use]
    pub const fn summary(&self) -> &SessionSummary {
        &self.summary
    }

    /// Strictly audit this closed stream for use as calibration evidence.
    ///
    /// Failure leaves the `ClosedSession` owned by the caller, so the rejected
    /// bytes remain available for an incident artifact.
    pub fn admit(&self) -> Result<FinalizedSession, SessionError> {
        FinalizedSession::audit(self.bytes.clone())
    }

    /// Consume and audit this stream without copying its retained bytes.
    pub fn into_admitted(self) -> Result<FinalizedSession, SessionError> {
        FinalizedSession::audit(self.bytes)
    }
}

/// Immutable session evidence admitted only after a complete strict audit.
///
/// Construction is private except for [`FinalizedSession::audit`]. This keeps
/// a header-only, record-prefix, or missing-footer byte stream out of APIs that
/// require finalized evidence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FinalizedSession {
    bytes: Arc<[u8]>,
    sha256: [u8; 32],
    header: SessionHeader,
    summary: SessionSummary,
    gate0a_terminal: Gate0aTerminalTracker,
}

impl FinalizedSession {
    /// Audit owned canonical bytes and admit them as finalized evidence.
    ///
    /// Invalid transport candidates remain valid evidence and are counted.
    /// Container corruption, truncation, trailing bytes, unsupported schemas,
    /// and source/payload hand mismatches fail closed.
    ///
    /// # Errors
    ///
    /// Returns the first strict schema, integrity, provenance, or I/O error.
    pub fn audit(bytes: impl Into<Vec<u8>>) -> Result<Self, SessionError> {
        let bytes = bytes.into();
        let mut gate0a_terminal = Gate0aTerminalTracker::default();
        let audit = audit_session_reader_with(bytes.as_slice(), |record_index, record, decode| {
            gate0a_terminal.observe(record_index, record, decode);
        })?;
        Ok(Self {
            bytes: Arc::from(bytes.into_boxed_slice()),
            sha256: audit.sha256,
            header: audit.header,
            summary: audit.summary,
            gate0a_terminal,
        })
    }

    /// Exact immutable canonical session bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Path-independent SHA-256 identity of [`Self::bytes`].
    #[must_use]
    pub const fn sha256(&self) -> [u8; 32] {
        self.sha256
    }

    /// Audited immutable session header.
    #[must_use]
    pub const fn header(&self) -> &SessionHeader {
        &self.header
    }

    /// Audited immutable stream accounting.
    #[must_use]
    pub const fn summary(&self) -> &SessionSummary {
        &self.summary
    }

    /// Iterate the already-audited records in append order.
    #[must_use]
    pub fn reader(&self) -> SessionReader<'_> {
        SessionReader::new(self)
    }

    /// Prove the exact Gate 0A ending from the bounded state retained during
    /// strict admission.
    ///
    /// # Errors
    ///
    /// Returns the first container, control binding, ordering, final-frame,
    /// source, or response-deadline failure.
    pub fn gate0a_terminal_binding(&self) -> Result<Gate0aTerminalBinding, Gate0aTerminalError> {
        self.gate0a_terminal.finish(&self.header, &self.summary)
    }
}

/// Audit canonical bytes into immutable finalized evidence.
///
/// This is the free-function form of [`FinalizedSession::audit`].
///
/// # Errors
///
/// Returns the first strict schema, integrity, provenance, or I/O error.
pub fn audit_session(bytes: impl Into<Vec<u8>>) -> Result<FinalizedSession, SessionError> {
    FinalizedSession::audit(bytes)
}

/// Strictly audit a complete session while retaining only one record payload.
///
/// The reader is consumed through the mandatory footer and EOF. Hashing,
/// container validation, provenance validation, and stream accounting happen
/// in the same pass, so file-backed live finalization has constant memory use.
///
/// # Errors
///
/// Returns the first strict schema, integrity, provenance, or I/O error.
pub fn audit_session_reader<R: Read>(reader: R) -> Result<SessionAudit, SessionError> {
    audit_session_reader_with(reader, |_, _, _| {})
}

/// Strictly audit IKS3 and prove its one-request, one-receipt Gate 0A ending.
///
/// This consumes the reader once and retains only ordinary stream accounting,
/// the final frame, and the two fixed-size control records.
pub fn audit_gate0a_session_reader<R: Read>(
    reader: R,
) -> Result<Gate0aSessionAudit, Gate0aTerminalError> {
    let mut terminal = Gate0aTerminalTracker::default();
    let session = audit_session_reader_with(reader, |record_index, record, decode| {
        terminal.observe(record_index, record, decode);
    })?;
    let binding = terminal.finish(session.header(), session.summary())?;
    Ok(Gate0aSessionAudit {
        session,
        terminal: binding,
    })
}

/// Strict audit with an observation hook for bounded derived-state builders.
///
/// The hook cannot authorize or reject bytes. The canonical parser and stream
/// accounting remain the only admission path, and they run to the footer even
/// when a caller records a separate derived-data failure.
pub(crate) fn audit_session_reader_with<R: Read>(
    reader: R,
    mut observe: impl FnMut(u64, &RawSessionRecord, CandidateDecode),
) -> Result<SessionAudit, SessionError> {
    let mut reader = RawSessionReader::new(HashingReader::new(reader))?;
    let header = *reader.header();
    let mut accounting = StreamAccounting::default();
    let mut record_index = 0_u64;
    while let Some(record) = reader.next_record()? {
        let observation = accounting.observe_candidate(
            record.source,
            record.kind,
            &record.payload,
            record.host_time_ns,
        );
        if let Some(claimed) = observation.claimed_hand_mismatch {
            return Err(SessionError::SourcePayloadHandMismatch {
                record_index,
                source: record.source,
                claimed,
            });
        }
        if let Some(error) = observation.invalid_quiesce {
            return Err(SessionError::InvalidQuiesceRecord {
                record_index,
                error,
            });
        }
        observe(record_index, &record, observation.decode);
        record_index += 1;
    }
    let (sha256, byte_len) = reader.into_inner().finish();
    Ok(SessionAudit {
        sha256,
        byte_len,
        header,
        summary: accounting.summary,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ObservedRequest {
    record_index: u64,
    source: ControllerStream,
    host_time_ns: u64,
    request: QuiesceRequest,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ObservedFrame {
    record_index: u64,
    source: ControllerStream,
    host_time_ns: u64,
    frame: SensorFrame,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ObservedReceipt {
    record_index: u64,
    source: ControllerStream,
    host_time_ns: u64,
    receipt: QuiesceReceipt,
    preceding_frame: Option<ObservedFrame>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct Gate0aTerminalTracker {
    source_mask: u8,
    request: Option<ObservedRequest>,
    receipt: Option<ObservedReceipt>,
    previous_frame: Option<ObservedFrame>,
    first_record_after_receipt: Option<u64>,
}

impl Gate0aTerminalTracker {
    fn observe(&mut self, record_index: u64, record: &RawSessionRecord, decode: CandidateDecode) {
        self.source_mask |= 1 << (record.source as u8);
        if self.receipt.is_some() && self.first_record_after_receipt.is_none() {
            self.first_record_after_receipt = Some(record_index);
        }

        let preceding_frame = self.previous_frame.take();
        if record.kind == SessionRecordKind::QuiesceRequest {
            let request = decode_quiesce_request(&record.payload)
                .expect("strict audit validates quiesce requests before observation");
            self.request.get_or_insert(ObservedRequest {
                record_index,
                source: record.source,
                host_time_ns: record.host_time_ns,
                request,
            });
            return;
        }
        if record.kind == SessionRecordKind::QuiesceReceipt {
            let receipt = decode_quiesce_receipt(&record.payload)
                .expect("strict audit validates quiesce receipts before observation");
            self.receipt.get_or_insert(ObservedReceipt {
                record_index,
                source: record.source,
                host_time_ns: record.host_time_ns,
                receipt,
                preceding_frame,
            });
            return;
        }
        match decode {
            CandidateDecode::Decoded(frame) => {
                self.previous_frame = Some(ObservedFrame {
                    record_index,
                    source: record.source,
                    host_time_ns: record.host_time_ns,
                    frame,
                });
            }
            CandidateDecode::NotDecoded | CandidateDecode::Invalid(_) => {}
        }
    }

    fn finish(
        self,
        header: &SessionHeader,
        summary: &SessionSummary,
    ) -> Result<Gate0aTerminalBinding, Gate0aTerminalError> {
        if summary.quiesce_requests != 1 || summary.quiesce_receipts != 1 {
            return Err(Gate0aTerminalError::ControlCount {
                requests: summary.quiesce_requests,
                receipts: summary.quiesce_receipts,
            });
        }
        if summary.invalid_records != 0 {
            return Err(Gate0aTerminalError::InvalidRecords {
                actual: summary.invalid_records,
            });
        }

        let request = self
            .request
            .expect("one strictly decoded request accompanies a count of one");
        let receipt = self
            .receipt
            .expect("one strictly decoded receipt accompanies a count of one");
        if let Some(trailing_record_index) = self.first_record_after_receipt {
            return Err(Gate0aTerminalError::ReceiptNotLast {
                receipt_record_index: receipt.record_index,
                trailing_record_index,
            });
        }
        let final_frame =
            receipt
                .preceding_frame
                .ok_or(Gate0aTerminalError::ReceiptWithoutImmediateFrame {
                    receipt_record_index: receipt.record_index,
                })?;
        if request.source != receipt.source || request.source != final_frame.source {
            return Err(Gate0aTerminalError::ControlSourceMismatch);
        }
        let expected_source_mask = 1 << (request.source as u8);
        if self.source_mask != expected_source_mask {
            return Err(Gate0aTerminalError::MultipleControllerStreams {
                source_mask: self.source_mask,
            });
        }
        if request.request.session_id != header.session_id {
            return Err(Gate0aTerminalError::HeaderSessionMismatch);
        }
        if request.host_time_ns != request.request.request_start_time_ns {
            return Err(Gate0aTerminalError::RequestRecordTimeMismatch {
                record_time_ns: request.host_time_ns,
                request_time_ns: request.request.request_start_time_ns,
            });
        }
        if request.request.hand != receipt.receipt.hand
            || request.request.controller_id_sha256 != receipt.receipt.controller_id_sha256
            || request.request.boot_id != receipt.receipt.boot_id
            || request.request.session_id != receipt.receipt.session_id
            || request.request.request_start_time_ns != receipt.receipt.request_start_time_ns
        {
            return Err(Gate0aTerminalError::ControlBindingMismatch);
        }
        let final_identity = final_frame.frame.identity();
        if final_identity.hand != request.request.hand
            || final_identity.controller_id_sha256 != request.request.controller_id_sha256
            || final_identity.boot_id != request.request.boot_id
        {
            return Err(Gate0aTerminalError::FinalFrameIdentityMismatch);
        }
        if final_frame.frame.flags != 0 || final_frame.frame.valid_sensor_mask != ALL_SENSORS_VALID
        {
            return Err(Gate0aTerminalError::FinalFrameNotClean {
                flags: final_frame.frame.flags,
                valid_sensor_mask: final_frame.frame.valid_sensor_mask,
            });
        }
        if final_frame.frame.sequence != receipt.receipt.final_sequence {
            return Err(Gate0aTerminalError::FinalSequenceMismatch {
                frame: final_frame.frame.sequence,
                receipt: receipt.receipt.final_sequence,
            });
        }
        if final_frame.host_time_ns < request.request.request_start_time_ns
            || receipt.host_time_ns < final_frame.host_time_ns
        {
            return Err(Gate0aTerminalError::TerminalHostTimeOrder {
                request_ns: request.request.request_start_time_ns,
                frame_ns: final_frame.host_time_ns,
                receipt_ns: receipt.host_time_ns,
            });
        }
        let response_time_ns = receipt.host_time_ns - request.request.request_start_time_ns;
        if response_time_ns > GATE0A_HOST_RESPONSE_TIMEOUT_NS {
            return Err(Gate0aTerminalError::ResponseTimeout {
                actual_ns: response_time_ns,
                maximum_ns: GATE0A_HOST_RESPONSE_TIMEOUT_NS,
            });
        }

        Ok(Gate0aTerminalBinding {
            source: request.source,
            request: request.request,
            receipt: receipt.receipt,
            final_frame: final_frame.frame,
            request_record_index: request.record_index,
            final_frame_record_index: final_frame.record_index,
            receipt_record_index: receipt.record_index,
            request_host_time_ns: request.host_time_ns,
            final_frame_host_time_ns: final_frame.host_time_ns,
            receipt_host_time_ns: receipt.host_time_ns,
            response_time_ns,
        })
    }
}

/// Write the fixed two-hand reference session used for file and replay tests.
///
/// The manifest hash is SHA-256 of the ASCII fixture identity
/// `invisible-keyboard-core28-v1`. This fixture exercises the container and
/// transport contracts; it contains no model of physical motion.
///
/// # Errors
///
/// Returns the first destination I/O error.
pub fn write_reference_session<W: Write>(
    writer: W,
    manifest_sha256: [u8; 32],
    nominal_period_us: u32,
    identities: [FrameIdentity; 2],
) -> Result<(W, SessionSummary), SessionError> {
    if identities[0].hand != Hand::Left || identities[1].hand != Hand::Right {
        return Err(SessionError::InvalidReferenceTopology);
    }
    let header = SessionHeader {
        nominal_period_us,
        manifest_sha256,
        session_id: [0x49; 16],
    };
    let mut session = SessionWriter::new(writer, header)?;
    for record_index in 0..REFERENCE_SESSION_RECORDS {
        let identity = if record_index % 2 == 0 {
            identities[0]
        } else {
            identities[1]
        };
        let hand = identity.hand;
        let sequence = record_index / 2;
        let sequence_value = i16::try_from(sequence).unwrap_or(i16::MAX);
        let hand_value = match hand {
            Hand::Left => -1,
            Hand::Right => 1,
        };
        let mut sensors = [SensorReading::default(); SENSOR_COUNT];
        for (sensor_index, sensor) in sensors.iter_mut().enumerate() {
            let sensor_value = i16::try_from(sensor_index).unwrap_or(i16::MAX);
            sensor.acceleration = [sequence_value, sensor_value, hand_value];
            sensor.angular_velocity = [-sequence_value, -sensor_value, -hand_value];
        }
        session.append_frame(
            ControllerStream::from(hand),
            &SensorFrame {
                hand,
                controller_id_sha256: identity.controller_id_sha256,
                boot_id: identity.boot_id,
                flags: 0,
                valid_sensor_mask: ALL_SENSORS_VALID,
                sequence,
                device_time_us: u64::from(sequence).saturating_mul(5_000),
                sensors,
            },
            u64::from(record_index).saturating_mul(2_500_000),
        )?;
    }
    session.finish()
}

/// One byte-exact raw record recovered from a session file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RawSessionRecord {
    /// Physical USB source that supplied the candidate bytes.
    pub source: ControllerStream,
    pub kind: SessionRecordKind,
    pub original_len: u64,
    pub host_time_ns: u64,
    pub payload: Vec<u8>,
}

/// Why a captured session record cannot become a canonical sensor frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecordDecodeError {
    CandidateNotDecoded(SessionRecordKind),
    Frame(FrameError),
    SourcePayloadHandMismatch {
        source: ControllerStream,
        claimed: Hand,
    },
}

impl fmt::Display for RecordDecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CandidateNotDecoded(kind) => {
                write!(formatter, "{kind:?} candidate cannot enter frame decoding")
            }
            Self::Frame(error) => error.fmt(formatter),
            Self::SourcePayloadHandMismatch { source, claimed } => write!(
                formatter,
                "decoded candidate from {source:?} stream claims {claimed:?} hand"
            ),
        }
    }
}

impl Error for RecordDecodeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Frame(error) => Some(error),
            Self::CandidateNotDecoded(_) | Self::SourcePayloadHandMismatch { .. } => None,
        }
    }
}

impl From<FrameError> for RecordDecodeError {
    fn from(error: FrameError) -> Self {
        Self::Frame(error)
    }
}

/// Why a control record cannot become its exact typed quiesce payload.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QuiesceRecordDecodeError {
    WrongKind {
        expected: SessionRecordKind,
        actual: SessionRecordKind,
    },
    Protocol(QuiesceRecordError),
    SourcePayloadHandMismatch {
        source: ControllerStream,
        claimed: Hand,
    },
}

impl fmt::Display for QuiesceRecordDecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongKind { expected, actual } => {
                write!(formatter, "expected {expected:?} record, found {actual:?}")
            }
            Self::Protocol(error) => error.fmt(formatter),
            Self::SourcePayloadHandMismatch { source, claimed } => write!(
                formatter,
                "control record from {source:?} stream claims {claimed:?} hand"
            ),
        }
    }
}

impl Error for QuiesceRecordDecodeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Protocol(error) => Some(error),
            Self::WrongKind { .. } | Self::SourcePayloadHandMismatch { .. } => None,
        }
    }
}

impl RawSessionRecord {
    /// Decode the captured payload without changing it.
    ///
    /// # Errors
    ///
    /// Returns a provenance error for any nondecoded candidate, or the
    /// canonical frame validation error for an invalid decoded payload.
    pub fn decode(&self) -> Result<SensorFrame, RecordDecodeError> {
        if self.kind != SessionRecordKind::Decoded {
            return Err(RecordDecodeError::CandidateNotDecoded(self.kind));
        }
        let frame = decode_frame(&self.payload)?;
        if frame.hand != self.source.expected_hand() {
            return Err(RecordDecodeError::SourcePayloadHandMismatch {
                source: self.source,
                claimed: frame.hand,
            });
        }
        Ok(frame)
    }

    /// Decode one exact outbound quiesce request from its owned record kind.
    pub fn decode_quiesce_request(&self) -> Result<QuiesceRequest, QuiesceRecordDecodeError> {
        if self.kind != SessionRecordKind::QuiesceRequest {
            return Err(QuiesceRecordDecodeError::WrongKind {
                expected: SessionRecordKind::QuiesceRequest,
                actual: self.kind,
            });
        }
        let request =
            decode_quiesce_request(&self.payload).map_err(QuiesceRecordDecodeError::Protocol)?;
        validate_control_source(self.source, request.hand)?;
        Ok(request)
    }

    /// Decode one exact inbound quiesce receipt from its owned record kind.
    pub fn decode_quiesce_receipt(&self) -> Result<QuiesceReceipt, QuiesceRecordDecodeError> {
        if self.kind != SessionRecordKind::QuiesceReceipt {
            return Err(QuiesceRecordDecodeError::WrongKind {
                expected: SessionRecordKind::QuiesceReceipt,
                actual: self.kind,
            });
        }
        let receipt =
            decode_quiesce_receipt(&self.payload).map_err(QuiesceRecordDecodeError::Protocol)?;
        validate_control_source(self.source, receipt.hand)?;
        Ok(receipt)
    }
}

fn validate_control_source(
    source: ControllerStream,
    claimed: Hand,
) -> Result<(), QuiesceRecordDecodeError> {
    if claimed != source.expected_hand() {
        return Err(QuiesceRecordDecodeError::SourcePayloadHandMismatch { source, claimed });
    }
    Ok(())
}

/// Reader wrapper that hashes exactly the bytes consumed by the strict parser.
struct HashingReader<R> {
    inner: R,
    hasher: Sha256,
    byte_len: u64,
}

impl<R> HashingReader<R> {
    fn new(inner: R) -> Self {
        Self {
            inner,
            hasher: Sha256::new(),
            byte_len: 0,
        }
    }

    fn finish(self) -> ([u8; 32], u64) {
        (self.hasher.finalize().into(), self.byte_len)
    }
}

impl<R: Read> Read for HashingReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let read = self.inner.read(buffer)?;
        self.hasher.update(&buffer[..read]);
        self.byte_len = self
            .byte_len
            .checked_add(u64::try_from(read).expect("read length fits u64"))
            .expect("one session cannot exceed u64 bytes");
        Ok(read)
    }
}

/// Internal streaming parser for untrusted session bytes.
struct RawSessionReader<R> {
    reader: R,
    header: SessionHeader,
    record_index: u64,
    finalized: bool,
}

impl<R: Read> RawSessionReader<R> {
    /// Read and validate the fixed session header.
    ///
    /// # Errors
    ///
    /// Returns a header validation error or I/O error. An incomplete header is
    /// always [`SessionError::TruncatedHeader`].
    pub fn new(mut reader: R) -> Result<Self, SessionError> {
        let mut bytes = [0_u8; HEADER_LEN];
        read_header_exact(&mut reader, &mut bytes)?;
        let header = decode_header(&bytes)?;
        Ok(Self {
            reader,
            header,
            record_index: 0,
            finalized: false,
        })
    }

    /// Return the immutable session header.
    #[must_use]
    pub const fn header(&self) -> &SessionHeader {
        &self.header
    }

    fn into_inner(self) -> R {
        self.reader
    }

    /// Read the next complete raw record, or `None` after the validated footer.
    ///
    /// # Errors
    ///
    /// Returns a record validation or I/O error. EOF before the mandatory
    /// footer is never treated as a complete session.
    pub fn next_record(&mut self) -> Result<Option<RawSessionRecord>, SessionError> {
        if self.finalized {
            return Ok(None);
        }

        let Some(magic) = read_entry_magic(&mut self.reader, self.record_index)? else {
            return Err(SessionError::MissingFooter {
                record_count: self.record_index,
            });
        };
        if &magic == FOOTER_MAGIC {
            self.read_footer(magic)?;
            self.finalized = true;
            return Ok(None);
        }
        if &magic != RECORD_MAGIC {
            return Err(SessionError::BadRecordMagic {
                record_index: self.record_index,
            });
        }

        let mut metadata = [0_u8; RECORD_METADATA_LEN];
        read_record_exact(&mut self.reader, &mut metadata, self.record_index)?;
        let payload_len = usize::try_from(u32::from_le_bytes(
            metadata[4..8].try_into().expect("four bytes"),
        ))
        .expect("u32 payload length fits usize");
        if payload_len > MAX_CAPTURE_PAYLOAD {
            return Err(SessionError::PayloadTooLarge {
                actual: payload_len,
            });
        }

        let mut payload = vec![0_u8; payload_len];
        let mut checksum_bytes = [0_u8; 4];
        read_record_exact(&mut self.reader, &mut payload, self.record_index)?;
        read_record_exact(&mut self.reader, &mut checksum_bytes, self.record_index)?;

        let stored = u32::from_le_bytes(checksum_bytes);
        let calculated = checksum_parts(&[&metadata, &payload]);
        if stored != calculated {
            return Err(SessionError::RecordChecksumMismatch {
                record_index: self.record_index,
                stored,
                calculated,
            });
        }

        let kind = SessionRecordKind::try_from(metadata[0]).map_err(|value| {
            SessionError::InvalidRecordKind {
                record_index: self.record_index,
                value,
            }
        })?;
        let source = ControllerStream::try_from(metadata[1]).map_err(|value| {
            SessionError::InvalidControllerStream {
                record_index: self.record_index,
                value,
            }
        })?;
        if metadata[2..4] != [0; 2] {
            return Err(SessionError::NonzeroRecordReserved {
                record_index: self.record_index,
            });
        }
        let original_len = u64::from_le_bytes(metadata[8..16].try_into().expect("eight bytes"));
        validate_candidate_length(kind, payload_len, original_len, Some(self.record_index))?;
        let host_time_ns = u64::from_le_bytes(metadata[16..24].try_into().expect("eight bytes"));

        self.record_index += 1;
        Ok(Some(RawSessionRecord {
            source,
            kind,
            original_len,
            host_time_ns,
            payload,
        }))
    }

    fn read_footer(&mut self, magic: [u8; 4]) -> Result<(), SessionError> {
        let mut footer = [0_u8; FOOTER_LEN];
        footer[..FOOTER_MAGIC.len()].copy_from_slice(&magic);
        read_footer_exact(&mut self.reader, &mut footer[FOOTER_MAGIC.len()..])?;

        let stored_checksum = u32::from_le_bytes(
            footer[FOOTER_CHECKSUM_OFFSET..]
                .try_into()
                .expect("four bytes"),
        );
        let calculated_checksum = checksum(&footer[..FOOTER_CHECKSUM_OFFSET]);
        if stored_checksum != calculated_checksum {
            return Err(SessionError::FooterChecksumMismatch {
                stored: stored_checksum,
                calculated: calculated_checksum,
            });
        }

        let stored_count = u64::from_le_bytes(footer[4..12].try_into().expect("eight bytes"));
        if stored_count != self.record_index {
            return Err(SessionError::FooterRecordCountMismatch {
                stored: stored_count,
                actual: self.record_index,
            });
        }

        let mut trailing = 0;
        if read_first_byte(&mut self.reader, &mut trailing)? {
            return Err(SessionError::TrailingBytes);
        }
        Ok(())
    }
}

/// Streaming replay over immutable, completely audited session evidence.
pub struct SessionReader<'a> {
    inner: RawSessionReader<&'a [u8]>,
}

impl<'a> SessionReader<'a> {
    /// Open an already-finalized session at its first record.
    #[must_use]
    pub fn new(session: &'a FinalizedSession) -> Self {
        // FinalizedSession construction has already validated this exact byte
        // slice. Building from the stored header avoids a second fallible
        // admission path while replay still checks each immutable record.
        Self {
            inner: RawSessionReader {
                reader: &session.bytes()[HEADER_LEN..],
                header: *session.header(),
                record_index: 0,
                finalized: false,
            },
        }
    }

    /// Return the immutable audited session header.
    #[must_use]
    pub const fn header(&self) -> &SessionHeader {
        self.inner.header()
    }

    /// Read the next complete raw record in append order.
    ///
    /// The session was audited before this reader could be constructed. An
    /// error here therefore reports an internal parser inconsistency rather
    /// than admitting new external bytes.
    ///
    /// # Errors
    ///
    /// Returns a strict parse error if replay disagrees with prior audit.
    pub fn next_record(&mut self) -> Result<Option<RawSessionRecord>, SessionError> {
        self.inner.next_record()
    }
}

/// Enforce the finite 1,200-second calibration/replay admission bound before a
/// caller allocates storage for a file.
///
/// Live and generic audit paths must use [`audit_session_reader`] instead.
pub fn validate_calibration_session_size(actual: u64) -> Result<(), SessionError> {
    if actual > MAX_SETUP_SESSION_BYTES {
        return Err(SessionError::CalibrationSessionTooLarge {
            actual,
            maximum: MAX_SETUP_SESSION_BYTES,
        });
    }
    Ok(())
}

/// Enforce the fixed 3,610-second Gate 0A admission bound before allocation.
///
/// Generic and live sessions do not use this acquisition-specific bound.
pub fn validate_gate0a_session_size(actual: u64) -> Result<(), SessionError> {
    if actual > MAX_GATE0A_SESSION_BYTES {
        return Err(SessionError::Gate0aSessionTooLarge {
            actual,
            maximum: MAX_GATE0A_SESSION_BYTES,
        });
    }
    Ok(())
}

/// Why a structurally valid IKS3 file does not prove the Gate 0A ending.
#[derive(Debug)]
pub enum Gate0aTerminalError {
    Session(SessionError),
    ControlCount {
        requests: u64,
        receipts: u64,
    },
    InvalidRecords {
        actual: u64,
    },
    ReceiptNotLast {
        receipt_record_index: u64,
        trailing_record_index: u64,
    },
    ReceiptWithoutImmediateFrame {
        receipt_record_index: u64,
    },
    ControlSourceMismatch,
    MultipleControllerStreams {
        source_mask: u8,
    },
    HeaderSessionMismatch,
    RequestRecordTimeMismatch {
        record_time_ns: u64,
        request_time_ns: u64,
    },
    ControlBindingMismatch,
    FinalFrameIdentityMismatch,
    FinalFrameNotClean {
        flags: u32,
        valid_sensor_mask: u8,
    },
    FinalSequenceMismatch {
        frame: u32,
        receipt: u32,
    },
    TerminalHostTimeOrder {
        request_ns: u64,
        frame_ns: u64,
        receipt_ns: u64,
    },
    ResponseTimeout {
        actual_ns: u64,
        maximum_ns: u64,
    },
}

impl fmt::Display for Gate0aTerminalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Session(error) => write!(formatter, "Gate 0A session audit failed: {error}"),
            Self::ControlCount { requests, receipts } => write!(
                formatter,
                "Gate 0A requires one request and one receipt; found {requests} requests and {receipts} receipts"
            ),
            Self::InvalidRecords { actual } => {
                write!(
                    formatter,
                    "Gate 0A session contains {actual} invalid records"
                )
            }
            Self::ReceiptNotLast {
                receipt_record_index,
                trailing_record_index,
            } => write!(
                formatter,
                "Gate 0A receipt record {receipt_record_index} is followed by record {trailing_record_index}"
            ),
            Self::ReceiptWithoutImmediateFrame {
                receipt_record_index,
            } => write!(
                formatter,
                "Gate 0A receipt record {receipt_record_index} does not immediately follow a sensor frame"
            ),
            Self::ControlSourceMismatch => {
                formatter.write_str("Gate 0A request, final frame, and receipt sources differ")
            }
            Self::MultipleControllerStreams { source_mask } => write!(
                formatter,
                "Gate 0A one-TTY session contains controller source mask {source_mask:#04b}"
            ),
            Self::HeaderSessionMismatch => {
                formatter.write_str("Gate 0A request session ID differs from the IKS3 header")
            }
            Self::RequestRecordTimeMismatch {
                record_time_ns,
                request_time_ns,
            } => write!(
                formatter,
                "Gate 0A request record time {record_time_ns} differs from request-start time {request_time_ns}"
            ),
            Self::ControlBindingMismatch => formatter
                .write_str("Gate 0A receipt does not exactly echo the request identity and time"),
            Self::FinalFrameIdentityMismatch => formatter
                .write_str("Gate 0A final sensor frame identity differs from the control records"),
            Self::FinalFrameNotClean {
                flags,
                valid_sensor_mask,
            } => write!(
                formatter,
                "Gate 0A final sensor frame has flags {flags:#010x} and validity mask {valid_sensor_mask:#010b}"
            ),
            Self::FinalSequenceMismatch { frame, receipt } => write!(
                formatter,
                "Gate 0A final frame sequence {frame} differs from receipt sequence {receipt}"
            ),
            Self::TerminalHostTimeOrder {
                request_ns,
                frame_ns,
                receipt_ns,
            } => write!(
                formatter,
                "Gate 0A terminal host times are out of order: request {request_ns}, frame {frame_ns}, receipt {receipt_ns}"
            ),
            Self::ResponseTimeout {
                actual_ns,
                maximum_ns,
            } => write!(
                formatter,
                "Gate 0A host response took {actual_ns} ns; maximum is {maximum_ns} ns"
            ),
        }
    }
}

impl Error for Gate0aTerminalError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Session(error) => Some(error),
            _ => None,
        }
    }
}

impl From<SessionError> for Gate0aTerminalError {
    fn from(error: SessionError) -> Self {
        Self::Session(error)
    }
}

/// Failures that make a session incomplete or untrustworthy.
#[derive(Debug)]
pub enum SessionError {
    Io(io::Error),
    Frame(FrameError),
    Quiesce(QuiesceRecordError),
    InvalidReferenceTopology,
    TruncatedHeader,
    BadHeaderMagic,
    UnsupportedHeaderVersion(u8),
    NonzeroHeaderReserved,
    HeaderChecksumMismatch {
        stored: u32,
        calculated: u32,
    },
    BadRecordMagic {
        record_index: u64,
    },
    InvalidRecordKind {
        record_index: u64,
        value: u8,
    },
    InvalidControllerStream {
        record_index: u64,
        value: u8,
    },
    NonzeroRecordReserved {
        record_index: u64,
    },
    TruncatedRecord {
        record_index: u64,
    },
    MissingFooter {
        record_count: u64,
    },
    TruncatedFooter,
    FooterChecksumMismatch {
        stored: u32,
        calculated: u32,
    },
    FooterRecordCountMismatch {
        stored: u64,
        actual: u64,
    },
    TrailingBytes,
    PayloadTooLarge {
        actual: usize,
    },
    CalibrationSessionTooLarge {
        actual: u64,
        maximum: u64,
    },
    Gate0aSessionTooLarge {
        actual: u64,
        maximum: u64,
    },
    InvalidCandidateLength {
        record_index: Option<u64>,
        kind: SessionRecordKind,
        captured: usize,
        original: u64,
    },
    RecordChecksumMismatch {
        record_index: u64,
        stored: u32,
        calculated: u32,
    },
    SourcePayloadHandMismatch {
        record_index: u64,
        source: ControllerStream,
        claimed: Hand,
    },
    InvalidQuiesceRecord {
        record_index: u64,
        error: QuiesceRecordError,
    },
}

impl fmt::Display for SessionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "session I/O failed: {error}"),
            Self::Frame(error) => write!(formatter, "session frame is invalid: {error}"),
            Self::Quiesce(error) => write!(formatter, "session quiesce record is invalid: {error}"),
            Self::InvalidReferenceTopology => {
                formatter.write_str("reference session identities must order left before right")
            }
            Self::TruncatedHeader => formatter.write_str("session header is truncated"),
            Self::BadHeaderMagic => formatter.write_str("session header has invalid magic"),
            Self::UnsupportedHeaderVersion(version) => {
                write!(formatter, "session version {version} is unsupported")
            }
            Self::NonzeroHeaderReserved => {
                formatter.write_str("session header reserved bytes are nonzero")
            }
            Self::HeaderChecksumMismatch { stored, calculated } => write!(
                formatter,
                "session header checksum {stored:#010x} does not match {calculated:#010x}"
            ),
            Self::BadRecordMagic { record_index } => {
                write!(formatter, "session record {record_index} has invalid magic")
            }
            Self::InvalidRecordKind {
                record_index,
                value,
            } => write!(
                formatter,
                "session record {record_index} has unknown candidate kind {value}"
            ),
            Self::InvalidControllerStream {
                record_index,
                value,
            } => write!(
                formatter,
                "session record {record_index} has unknown controller stream {value}"
            ),
            Self::NonzeroRecordReserved { record_index } => write!(
                formatter,
                "session record {record_index} has nonzero reserved bytes"
            ),
            Self::TruncatedRecord { record_index } => {
                write!(formatter, "session record {record_index} is truncated")
            }
            Self::MissingFooter { record_count } => write!(
                formatter,
                "session ended after {record_count} records without its completion footer"
            ),
            Self::TruncatedFooter => formatter.write_str("session completion footer is truncated"),
            Self::FooterChecksumMismatch { stored, calculated } => write!(
                formatter,
                "session footer checksum {stored:#010x} does not match {calculated:#010x}"
            ),
            Self::FooterRecordCountMismatch { stored, actual } => write!(
                formatter,
                "session footer declares {stored} records but replay found {actual}"
            ),
            Self::TrailingBytes => {
                formatter.write_str("session contains bytes after its completion footer")
            }
            Self::PayloadTooLarge { actual } => write!(
                formatter,
                "captured payload is {actual} bytes; maximum is {MAX_CAPTURE_PAYLOAD}"
            ),
            Self::CalibrationSessionTooLarge { actual, maximum } => write!(
                formatter,
                "calibration session is {actual} bytes; maximum for the {SETUP_TIME_CAP_SECONDS}-second setup contract is {maximum}"
            ),
            Self::Gate0aSessionTooLarge { actual, maximum } => write!(
                formatter,
                "Gate 0A session is {actual} bytes; maximum for the {GATE0A_CAPTURE_TIME_SECONDS}-second acquisition contract is {maximum}"
            ),
            Self::InvalidCandidateLength {
                record_index,
                kind,
                captured,
                original,
            } => {
                if let Some(record_index) = record_index {
                    write!(
                        formatter,
                        "session record {record_index} has invalid {kind:?} lengths: captured {captured}, original {original}"
                    )
                } else {
                    write!(
                        formatter,
                        "cannot append {kind:?} candidate with captured length {captured} and original length {original}"
                    )
                }
            }
            Self::RecordChecksumMismatch {
                record_index,
                stored,
                calculated,
            } => write!(
                formatter,
                "session record {record_index} checksum {stored:#010x} does not match {calculated:#010x}"
            ),
            Self::SourcePayloadHandMismatch {
                record_index,
                source,
                claimed,
            } => write!(
                formatter,
                "session record {record_index} came from {source:?} stream but claims {claimed:?} hand"
            ),
            Self::InvalidQuiesceRecord {
                record_index,
                error,
            } => write!(
                formatter,
                "session quiesce record {record_index} is invalid: {error}"
            ),
        }
    }
}

impl Error for SessionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Frame(error) => Some(error),
            Self::Quiesce(error) => Some(error),
            Self::InvalidQuiesceRecord { error, .. } => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for SessionError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<FrameError> for SessionError {
    fn from(error: FrameError) -> Self {
        Self::Frame(error)
    }
}

impl From<QuiesceRecordError> for SessionError {
    fn from(error: QuiesceRecordError) -> Self {
        Self::Quiesce(error)
    }
}

fn validate_candidate_length(
    kind: SessionRecordKind,
    captured: usize,
    original: u64,
    record_index: Option<u64>,
) -> Result<(), SessionError> {
    let captured_u64 = u64::try_from(captured).unwrap_or(u64::MAX);
    let valid = match kind {
        SessionRecordKind::Oversized => original > captured_u64,
        SessionRecordKind::Decoded
        | SessionRecordKind::Malformed
        | SessionRecordKind::Unterminated
        | SessionRecordKind::QuiesceRequest
        | SessionRecordKind::QuiesceReceipt => original == captured_u64,
    };
    if valid {
        return Ok(());
    }
    Err(SessionError::InvalidCandidateLength {
        record_index,
        kind,
        captured,
        original,
    })
}

fn encode_header(header: SessionHeader) -> [u8; HEADER_LEN] {
    let mut bytes = [0_u8; HEADER_LEN];
    bytes[0..4].copy_from_slice(SESSION_MAGIC);
    bytes[4] = SESSION_VERSION;
    bytes[8..12].copy_from_slice(&header.nominal_period_us.to_le_bytes());
    bytes[12..44].copy_from_slice(&header.manifest_sha256);
    bytes[44..60].copy_from_slice(&header.session_id);
    let checksum = checksum(&bytes[..HEADER_CHECKSUM_OFFSET]);
    bytes[HEADER_CHECKSUM_OFFSET..].copy_from_slice(&checksum.to_le_bytes());
    bytes
}

fn encode_footer(record_count: u64) -> [u8; FOOTER_LEN] {
    let mut bytes = [0_u8; FOOTER_LEN];
    bytes[..FOOTER_MAGIC.len()].copy_from_slice(FOOTER_MAGIC);
    bytes[4..12].copy_from_slice(&record_count.to_le_bytes());
    let checksum = checksum(&bytes[..FOOTER_CHECKSUM_OFFSET]);
    bytes[FOOTER_CHECKSUM_OFFSET..].copy_from_slice(&checksum.to_le_bytes());
    bytes
}

fn decode_header(bytes: &[u8; HEADER_LEN]) -> Result<SessionHeader, SessionError> {
    let stored = u32::from_le_bytes(
        bytes[HEADER_CHECKSUM_OFFSET..]
            .try_into()
            .expect("four bytes"),
    );
    let calculated = checksum(&bytes[..HEADER_CHECKSUM_OFFSET]);
    if stored != calculated {
        return Err(SessionError::HeaderChecksumMismatch { stored, calculated });
    }
    if &bytes[0..4] != SESSION_MAGIC {
        return Err(SessionError::BadHeaderMagic);
    }
    if bytes[4] != SESSION_VERSION {
        return Err(SessionError::UnsupportedHeaderVersion(bytes[4]));
    }
    if bytes[5..8] != [0; 3] {
        return Err(SessionError::NonzeroHeaderReserved);
    }

    Ok(SessionHeader {
        nominal_period_us: u32::from_le_bytes(bytes[8..12].try_into().expect("four bytes")),
        manifest_sha256: bytes[12..44].try_into().expect("32 bytes"),
        session_id: bytes[44..60].try_into().expect("16 bytes"),
    })
}

fn read_header_exact(reader: &mut impl Read, bytes: &mut [u8]) -> Result<(), SessionError> {
    reader.read_exact(bytes).map_err(|error| {
        if error.kind() == io::ErrorKind::UnexpectedEof {
            SessionError::TruncatedHeader
        } else {
            SessionError::Io(error)
        }
    })
}

fn read_record_exact(
    reader: &mut impl Read,
    bytes: &mut [u8],
    record_index: u64,
) -> Result<(), SessionError> {
    reader.read_exact(bytes).map_err(|error| {
        if error.kind() == io::ErrorKind::UnexpectedEof {
            SessionError::TruncatedRecord { record_index }
        } else {
            SessionError::Io(error)
        }
    })
}

fn read_footer_exact(reader: &mut impl Read, bytes: &mut [u8]) -> Result<(), SessionError> {
    reader.read_exact(bytes).map_err(|error| {
        if error.kind() == io::ErrorKind::UnexpectedEof {
            SessionError::TruncatedFooter
        } else {
            SessionError::Io(error)
        }
    })
}

fn read_entry_magic(
    reader: &mut impl Read,
    record_index: u64,
) -> Result<Option<[u8; 4]>, SessionError> {
    let mut magic = [0_u8; 4];
    let mut length = 0;
    while length < magic.len() {
        match reader.read(&mut magic[length..]) {
            Ok(0) if length == 0 => return Ok(None),
            Ok(0) if FOOTER_MAGIC.starts_with(&magic[..length]) => {
                return Err(SessionError::TruncatedFooter);
            }
            Ok(0) => return Err(SessionError::TruncatedRecord { record_index }),
            Ok(read) => length += read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(SessionError::Io(error)),
        }
    }
    Ok(Some(magic))
}

fn read_first_byte(reader: &mut impl Read, byte: &mut u8) -> Result<bool, SessionError> {
    let mut buffer = [0_u8; 1];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => return Ok(false),
            Ok(1) => {
                *byte = buffer[0];
                return Ok(true);
            }
            Ok(_) => unreachable!("one-byte read returned more than one byte"),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(SessionError::Io(error)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one_record_session() -> Vec<u8> {
        let header = SessionHeader {
            nominal_period_us: 5_000,
            manifest_sha256: [1; 32],
            session_id: [2; 16],
        };
        let mut writer = SessionWriter::new(Vec::new(), header).expect("header writes");
        writer
            .append_candidate(
                ControllerStream::Left,
                SessionRecordKind::Decoded,
                &[1, 2, 3],
                3,
                7,
            )
            .expect("candidate writes");
        writer.finish().expect("session finishes").0
    }

    fn audited_timing_session(left_times: [(u64, u64); 2]) -> FinalizedSession {
        let header = SessionHeader {
            nominal_period_us: 5_000,
            manifest_sha256: [1; 32],
            session_id: [2; 16],
        };
        let mut writer = SessionWriter::new(Vec::new(), header).expect("session starts");
        for (sequence, (left_device_time_us, left_host_time_ns)) in
            left_times.into_iter().enumerate()
        {
            for (hand, device_time_us, host_time_ns, identity) in [
                (Hand::Left, left_device_time_us, left_host_time_ns, 0x11),
                (
                    Hand::Right,
                    100 + u64::try_from(sequence).expect("sequence fits u64") * 100,
                    1 + u64::try_from(sequence).expect("sequence fits u64"),
                    0x22,
                ),
            ] {
                writer
                    .append_frame(
                        ControllerStream::from(hand),
                        &SensorFrame {
                            hand,
                            controller_id_sha256: [identity; 32],
                            boot_id: [identity; 16],
                            flags: 0,
                            valid_sensor_mask: ALL_SENSORS_VALID,
                            sequence: u32::try_from(sequence).expect("sequence fits u32"),
                            device_time_us,
                            sensors: [SensorReading::default(); SENSOR_COUNT],
                        },
                        host_time_ns,
                    )
                    .expect("frame writes");
            }
        }
        writer.finish_finalized().expect("complete session audits")
    }

    fn rewrite_first_record_checksum(bytes: &mut [u8]) {
        let payload_len = 3;
        let checksum_offset = HEADER_LEN + RECORD_PREFIX_LEN + payload_len;
        let checksum = checksum(&bytes[HEADER_LEN + RECORD_MAGIC.len()..checksum_offset]);
        bytes[checksum_offset..checksum_offset + 4].copy_from_slice(&checksum.to_le_bytes());
    }

    #[test]
    fn all_header_prefix_corruption_is_detected() {
        let header = SessionHeader {
            nominal_period_us: 5_000,
            manifest_sha256: [1; 32],
            session_id: [2; 16],
        };
        let encoded = encode_header(header);

        for byte_index in 0..HEADER_CHECKSUM_OFFSET {
            for bit_index in 0..8 {
                let mut corrupted = encoded;
                corrupted[byte_index] ^= 1 << bit_index;
                assert!(decode_header(&corrupted).is_err());
            }
        }
    }

    #[test]
    fn oversized_payload_is_rejected_before_the_writer_changes() {
        let header = SessionHeader {
            nominal_period_us: 5_000,
            manifest_sha256: [1; 32],
            session_id: [2; 16],
        };
        let mut writer = SessionWriter::new(Vec::new(), header).expect("header writes");

        assert!(matches!(
            writer.append_candidate(
                ControllerStream::Left,
                SessionRecordKind::Decoded,
                &vec![0; MAX_CAPTURE_PAYLOAD + 1],
                u64::try_from(MAX_CAPTURE_PAYLOAD + 1).expect("limit fits u64"),
                1,
            ),
            Err(SessionError::PayloadTooLarge { .. })
        ));
        let (bytes, summary) = writer.finish().expect("session finishes");
        assert_eq!(bytes.len(), HEADER_LEN + FOOTER_LEN);
        assert_eq!(summary.total_records, 0);
    }

    #[test]
    fn checksum_valid_footer_count_mismatch_is_rejected() {
        let mut bytes = one_record_session();
        let footer_offset = bytes.len() - FOOTER_LEN;
        bytes[footer_offset + 4..footer_offset + 12].copy_from_slice(&2_u64.to_le_bytes());
        let checksum = checksum(&bytes[footer_offset..footer_offset + FOOTER_CHECKSUM_OFFSET]);
        bytes[footer_offset + FOOTER_CHECKSUM_OFFSET..].copy_from_slice(&checksum.to_le_bytes());

        assert!(matches!(
            audit_session(bytes.as_slice()),
            Err(SessionError::FooterRecordCountMismatch {
                stored: 2,
                actual: 1,
            })
        ));
    }

    #[test]
    fn checksum_valid_unknown_record_kind_is_rejected() {
        let mut bytes = one_record_session();
        bytes[HEADER_LEN + 4] = 0xff;
        rewrite_first_record_checksum(&mut bytes);
        let mut reader = RawSessionReader::new(bytes.as_slice()).expect("header remains valid");

        assert!(matches!(
            reader.next_record(),
            Err(SessionError::InvalidRecordKind {
                record_index: 0,
                value: 0xff,
            })
        ));
    }

    #[test]
    fn checksum_valid_nonzero_record_reserved_bytes_are_rejected() {
        let mut bytes = one_record_session();
        bytes[HEADER_LEN + 6] = 1;
        rewrite_first_record_checksum(&mut bytes);
        let mut reader = RawSessionReader::new(bytes.as_slice()).expect("header remains valid");

        assert!(matches!(
            reader.next_record(),
            Err(SessionError::NonzeroRecordReserved { record_index: 0 })
        ));
    }

    #[test]
    fn checksum_valid_candidate_length_contradiction_is_rejected() {
        let mut bytes = one_record_session();
        bytes[HEADER_LEN + 12..HEADER_LEN + 20].copy_from_slice(&4_u64.to_le_bytes());
        rewrite_first_record_checksum(&mut bytes);
        let mut reader = RawSessionReader::new(bytes.as_slice()).expect("header remains valid");

        assert!(matches!(
            reader.next_record(),
            Err(SessionError::InvalidCandidateLength {
                record_index: Some(0),
                kind: SessionRecordKind::Decoded,
                captured: 3,
                original: 4,
            })
        ));
    }

    #[test]
    fn clean_two_hand_stream_rejects_recorded_time_regressions() {
        let clean = audited_timing_session([(100, 1), (200, 2)]);
        assert!(clean.summary().is_clean_two_hand_sensor_stream());

        let device_regression = audited_timing_session([(100, 1), (99, 2)]);
        assert_eq!(device_regression.summary().left.device_time_regressions, 1);
        assert!(
            !device_regression
                .summary()
                .is_clean_two_hand_sensor_stream()
        );

        let host_regression = audited_timing_session([(100, 2), (200, 1)]);
        assert_eq!(host_regression.summary().left.host_time_regressions, 1);
        assert!(!host_regression.summary().is_clean_two_hand_sensor_stream());
    }
}
