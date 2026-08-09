//! Direct, streaming reduction of decisive Gate 0A analyzer evidence.
//!
//! The decisive artifact is libsigrok `binary` output from the pinned DSLogic
//! path, not a decoded report. It is a continuous little-endian stream of
//! 16-bit samples at 2 MHz. Bits 0 through 5 are the six INT1 lines, bit 6 is
//! D8, bit 7 is D9, bit 8 is SDA, and bit 9 is SCL. Bits 10 through 15 must be
//! zero. The reducer derives every transition and decodes I2C itself with
//! constant working state.
//!
//! A bounded JSON sidecar binds the raw file, the actual capture binary, and
//! the full capture log by SHA-256. It contains settings and provenance only.
//! It contains no read counts, timing totals, decoded transfers, or pass/fail
//! claims. The patched capture driver must fail closed on USB transfer errors
//! and emit one machine-readable success summary in the hash-bound log.
//!
//! The sidecar uses this exact, unknown-field-rejecting shape:
//!
//! ```json
//! {
//!   "schema": "invisible-keyboard-gate0-dslogic-capture-v1",
//!   "raw_capture_sha256": "<64 lowercase hex digits>",
//!   "raw_capture_bytes": 14440000000,
//!   "word_bytes": 2,
//!   "byte_order": "little",
//!   "sample_rate_hz": 2000000,
//!   "channel_map": {"int1":[0,1,2,3,4,5],"d8":6,"d9":7,"sda":8,"scl":9},
//!   "voltage_threshold_low_mv": 1400,
//!   "voltage_threshold_high_mv": 3600,
//!   "requested_samples": 7220000000,
//!   "capture_started_unix_ms": 0,
//!   "capture_finished_unix_ms": 3610000,
//!   "capture_binary": {"path":"capture.sigrok-cli","sha256":"<sha256>","bytes":1},
//!   "capture_log": {"path":"capture.log","sha256":"<sha256>","bytes":1},
//!   "device_firmware_bundle": {"path":"firmware.bundle","sha256":"<sha256>","bytes":1},
//!   "driver_base_commit": "0bc2487778e660f4d3116729b6f4aee2b1996bb0",
//!   "sigrok_cli_base_commit": "f44dd91347e7ac797cefc23162b9fcf0b7329f1f",
//!   "driver_patch_sha256": "<sha256>",
//!   "device_model": "DSLogic Plus",
//!   "capture_exit_status": 0,
//!   "command": [".capture.sigrok-cli.partial","--driver","dreamsourcelab-dslogic","--channels","0,1,2,3,4,5,6,7,8,9","--config","samplerate=2m:voltage_threshold=1.4-3.6:continuous=true","--samples","7220000000","--output-format","binary","--output-file",".capture.raw.partial","--loglevel","4"]
//! }
//! ```
//!
//! Artifact paths are relative to the sidecar directory and cannot escape it,
//! including through a symlink. The two timestamps are Unix milliseconds and
//! the finish must be later than the start. The capture command requires each
//! decisive option exactly once and rejects `--time` and the input-only
//! `--format` option. The hash-bound log contains exactly one LF-terminated:
//!
//! ```text
//! IKB_GATE0_CAPTURE_V2 samples=7220000000 completed_transfers=<positive-u64> anomalies=0 stop_observed_raw_bytes=<14416000000..14417000000> session_bytes=<exact-IKS3-bytes> locked_manifest_sha256=<64-lowercase-hex> firmware_bundle_sha256=<64-lowercase-hex>
//! ```
//! It also contains exactly one matching patched-driver receipt and exactly
//! one receipt for each fixed DSLogic Plus firmware image:
//!
//! ```text
//! IKB_DSLOGIC_TRANSFER_V1 completed_transfers=<same-positive-u64> anomalies=0
//! IKB_GATE0_FIRMWARE_V1 name=dreamsourcelab-dslogic-plus-fpga.fw bytes=341160 sha256=e16c70e4af9181044e3ec3a363e4bb109f1d731b4f9f046f8bc1d1015a7842a7
//! IKB_GATE0_FIRMWARE_V1 name=dreamsourcelab-dslogic-plus-fx2.fw bytes=8120 sha256=9197e08dd33a9cda61cc8cd3de559cbae9b049b2dfa827c089ffe0528f5dcc5e
//! ```
//!
//! The controller emits one startup anchor before IMU initialization: D8 high
//! for at least 20 us, D8 low for at least 20 us, D9 high for at least 20 us,
//! then D9 low for at least 20 us. Normal D8 and D9 pulses are at least 2 us
//! and shorter than 20 us. The anchor is excluded from snapshot and commit
//! counts. Analyzer snapshot ordinal N then maps directly to firmware sequence
//! N, including host-dropped sequence gaps. Because capture starts before
//! power-on, the reducer permits the SDA and SCL pull-ups to settle until both
//! lines first reach idle-high. It rejects every later bus edge before D8.
//!
//! Direct parsing removes caller-authored decoding from the decisive path. The
//! remaining physical trust boundary is the analyzer, probes, sample clock,
//! pinned vendor firmware bundle, and pinned fail-closed libsigrok/DSLogic
//! build. Hashes prove artifact identity, not that the instrument observed
//! every physical transition.

use std::{
    error::Error,
    fmt,
    fs::File,
    io::{self, BufRead, BufReader, Read},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    manifest::{AcquisitionManifest, ManifestError, format_sha256, parse_sha256, sha256},
    protocol::{ALL_SENSORS_VALID, FrameIdentity, SENSOR_COUNT, SensorFrame, SensorReading},
    session::{
        ControllerStream, FinalizedSession, Gate0aTerminalError, SessionError, SessionReader,
        SessionRecordKind,
    },
};

/// Exact generated metadata schema accepted next to the raw capture.
pub const GATE0_CAPTURE_SIDECAR_SCHEMA: &str = "invisible-keyboard-gate0-dslogic-capture-v1";

/// Fixed DSLogic sampling rate used by the canonical raw stream.
pub const GATE0_SAMPLE_RATE_HZ: u64 = 2_000_000;

/// Lower half of the canonical DSLogic threshold range.
pub const GATE0_VOLTAGE_THRESHOLD_LOW_MV: u32 = 1_400;

/// Upper half of the canonical DSLogic threshold range.
pub const GATE0_VOLTAGE_THRESHOLD_HIGH_MV: u32 = 3_600;

/// Canonical patched-driver request, including a nine-second margin.
pub const GATE0_REQUESTED_SAMPLES: u64 = 7_220_000_000;

/// Minimum raw coverage for the decisive 3,601-second interval.
pub const GATE0_MINIMUM_SAMPLES: u64 = 7_202_000_000;

/// Maximum accepted sidecar size.
pub const MAX_GATE0_CAPTURE_SIDECAR_BYTES: u64 = 1_048_576;

/// Maximum path or command string in the sidecar.
pub const MAX_GATE0_CAPTURE_STRING_BYTES: usize = 4_096;

/// Maximum detailed violation witnesses kept in the compact report.
pub const MAX_GATE0_TRACE_VIOLATION_WITNESSES: usize = 64;

/// Nominal sensor output-data rate in millihertz.
pub const GATE0_NOMINAL_ODR_MILLIHZ: u64 = 208_000;

/// Provisional project ODR lower bound, minus three percent.
///
/// The inspected ST datasheet names 208 Hz but does not guarantee numeric ODR
/// accuracy. This project tolerance must be replaced when measured prototype
/// data or a guaranteed source supports a different bound.
pub const GATE0_PROVISIONAL_ODR_MIN_MILLIHZ: u64 = 201_760;

/// Provisional project ODR upper bound, plus three percent.
pub const GATE0_PROVISIONAL_ODR_MAX_MILLIHZ: u64 = 214_240;

const RAW_WORD_BYTES: u8 = 2;
const RAW_BYTE_ORDER: &str = "little";
const USED_LOGIC_MASK: u16 = 0x03ff;
const INT1_MASK: u16 = 0x003f;
const D8_MASK: u16 = 1 << 6;
const D9_MASK: u16 = 1 << 7;
const SDA_MASK: u16 = 1 << 8;
const SCL_MASK: u16 = 1 << 9;
const TICKS_PER_MICROSECOND: u64 = 2;
const NORMAL_MARKER_MIN_TICKS: u64 = 2 * TICKS_PER_MICROSECOND;
const BOOT_MARKER_MIN_TICKS: u64 = 20 * TICKS_PER_MICROSECOND;
const BOOT_MARKER_MAX_TICKS: u64 = 200 * TICKS_PER_MICROSECOND;
const TERMINAL_MARKER_MIN_TICKS: u64 = 100 * TICKS_PER_MICROSECOND;
const TERMINAL_MARKER_MAX_TICKS: u64 = 200 * TICKS_PER_MICROSECOND;
const TERMINAL_PREMARKER_IDLE_TICKS: u64 = 200 * TICKS_PER_MICROSECOND;
const TERMINAL_IDLE_MINIMUM_SAMPLES: u64 = 10_000_000;
const TERMINAL_MARKER_FALL_DEADLINE: u64 = 7_210_000_000;
const TERMINAL_IDLE_WORD: u16 = SDA_MASK | SCL_MASK;
const TERMINAL_MARKER_WORD: u16 = TERMINAL_IDLE_WORD | D8_MASK | D9_MASK;
const SHUTDOWN_TRANSACTION_COUNT: usize = 52;
const SHUTDOWN_SCL_RISING_EDGES: u64 = 1_422;
const MUX_ADDRESS: u8 = 0x70;
const IMU_ADDRESS: u8 = 0x6a;
const SHUTDOWN_REGISTERS: [u8; 3] = [0x0d, 0x10, 0x11];
const STARTUP_PROFILE: [(u8, u8); 5] = [
    (0x12, 0x44),
    (0x10, 0x58),
    (0x11, 0x58),
    (0x0b, 0x80),
    (0x0d, 0x00),
];
const RESET_REGISTER: u8 = 0x12;
const RESET_MASK: u8 = 0x01;
const WHO_AM_I_REGISTER: u8 = 0x0f;
const WHO_AM_I_VALUE: u8 = 0x6a;
const RESET_POLL_LIMIT: u8 = 100;
const RESET_POLL_DELAY_TICKS: u64 = 100 * TICKS_PER_MICROSECOND;
const MUX_SETTLE_TICKS: u64 = 10 * TICKS_PER_MICROSECOND;
const SENSOR_PRIMING_MAX_TICKS: u64 = 100_000 * TICKS_PER_MICROSECOND;
const STATUS_REGISTER: u8 = 0x1e;
const MOTION_REGISTER: u8 = 0x22;
const STATUS_READY_MASK: u8 = 0x03;
const MAX_STATUS_POLLS: usize = 20;
const MAX_I2C_BYTES: usize = 16;
const MAX_SAMPLE_SPREAD_TICKS: u64 = 5_000 * TICKS_PER_MICROSECOND;
const MAX_SAMPLE_AGE_TICKS: u64 = 5_000 * TICKS_PER_MICROSECOND;
const SAMPLE_OFFSET_TOLERANCE_TICKS: i128 = 100 * TICKS_PER_MICROSECOND as i128;
const PHASE_BIN_WIDTH_TICKS: u64 = 250 * TICKS_PER_MICROSECOND;
const PHASE_BIN_COUNT: usize = 20;
const ODR_INTERVAL_NUMERATOR: u64 = GATE0_SAMPLE_RATE_HZ * 1_000;
const MIN_PROVISIONAL_ODR_INTERVAL_TICKS: u64 =
    ODR_INTERVAL_NUMERATOR / (GATE0_PROVISIONAL_ODR_MAX_MILLIHZ + 1) + 1;
const MAX_PROVISIONAL_ODR_INTERVAL_TICKS: u64 =
    ODR_INTERVAL_NUMERATOR / GATE0_PROVISIONAL_ODR_MIN_MILLIHZ;
const PATCHED_DRIVER_BASE_COMMIT: &str = "0bc2487778e660f4d3116729b6f4aee2b1996bb0";
const PATCHED_SIGROK_CLI_BASE_COMMIT: &str = "f44dd91347e7ac797cefc23162b9fcf0b7329f1f";
const GATE0_DEVICE_MODEL: &str = "DSLogic Plus";
const STOP_WINDOW_FIRST_BYTE: u64 = 14_416_000_000;
const STOP_WINDOW_LAST_BYTE: u64 = 14_417_000_000;
const CAPTURE_LOG_SUMMARY_PREFIX: &str = "IKB_GATE0_CAPTURE_V2 ";
const DRIVER_RECEIPT_PREFIX: &str = "IKB_DSLOGIC_TRANSFER_V1 ";
const FIRMWARE_RECEIPTS: [&str; 2] = [
    "IKB_GATE0_FIRMWARE_V1 name=dreamsourcelab-dslogic-plus-fpga.fw bytes=341160 sha256=e16c70e4af9181044e3ec3a363e4bb109f1d731b4f9f046f8bc1d1015a7842a7",
    "IKB_GATE0_FIRMWARE_V1 name=dreamsourcelab-dslogic-plus-fx2.fw bytes=8120 sha256=9197e08dd33a9cda61cc8cd3de559cbae9b049b2dfa827c089ffe0528f5dcc5e",
];

/// Native artifacts opened by the canonical Gate 0A scorer.
#[derive(Clone, Copy, Debug)]
pub struct Gate0CapturePaths<'a> {
    pub raw_capture: &'a Path,
    pub sidecar: &'a Path,
}

/// Fixed channel-to-bit map recorded in the generated sidecar.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Gate0ChannelMap {
    pub int1: [u8; SENSOR_COUNT],
    pub d8: u8,
    pub d9: u8,
    pub sda: u8,
    pub scl: u8,
}

impl Gate0ChannelMap {
    const fn canonical() -> Self {
        Self {
            int1: [0, 1, 2, 3, 4, 5],
            d8: 6,
            d9: 7,
            sda: 8,
            scl: 9,
        }
    }
}

/// Hash-bound capture configuration copied into the compact Gate 0A report.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Gate0CaptureSettings {
    pub sample_rate_hz: u64,
    pub word_bytes: u8,
    pub byte_order: String,
    pub channel_map: Gate0ChannelMap,
    pub voltage_threshold_low_mv: u32,
    pub voltage_threshold_high_mv: u32,
    pub requested_samples: u64,
    pub capture_started_unix_ms: u64,
    pub capture_finished_unix_ms: u64,
    pub capture_binary_sha256: [u8; 32],
    pub capture_log_sha256: [u8; 32],
    pub device_firmware_bundle_sha256: [u8; 32],
    pub driver_base_commit: String,
    pub sigrok_cli_base_commit: String,
    pub driver_patch_sha256: [u8; 32],
    pub device_model: String,
}

/// Full-capture metrics derived from raw samples by this repository.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Gate0TraceMetrics {
    pub raw_samples: u64,
    pub raw_transitions: u64,
    pub decoded_i2c_transactions: u64,
    pub startup_transactions: u64,
    pub startup_unadmitted_int1_edges: [u64; SENSOR_COUNT],
    pub startup_end_sample: Option<u64>,
    pub first_clean_cache_sample: Option<u64>,
    pub scl_rising_edges: u64,
    pub minimum_scl_period_ticks: Option<u64>,
    pub maximum_scl_period_ticks: Option<u64>,
    pub int1_edges: [u64; SENSOR_COUNT],
    pub completed_reads: [u64; SENSOR_COUNT],
    pub committed_reads: [u64; SENSOR_COUNT],
    pub selected_reads: [u64; SENSOR_COUNT],
    pub linked_reads: [u64; SENSOR_COUNT],
    pub host_dropped_reads: [u64; SENSOR_COUNT],
    pub decimated_reads: [u64; SENSOR_COUNT],
    pub pre_snapshot_decimated_reads: [u64; SENSOR_COUNT],
    pub snapshots: u64,
    pub linked_snapshots: u64,
    pub host_dropped_snapshots: u64,
    pub status_poll_histogram: [u64; MAX_STATUS_POLLS],
    pub total_status_polls: u64,
    pub average_status_polls_milli: Option<u64>,
    pub phase_histogram: [[u64; PHASE_BIN_COUNT]; SENSOR_COUNT],
    pub odr_millihz: [Option<u64>; SENSOR_COUNT],
    pub interval_jitter_rms_ticks: [Option<u64>; SENSOR_COUNT],
    pub max_read_latency_ticks: u64,
    pub max_i2c_to_d9_commit_ticks: u64,
    pub max_selected_sample_age_ticks: u64,
    pub max_snapshot_spread_ticks: u64,
    pub max_pending_reads: u8,
    pub next_edge_crossings: u64,
    pub final_sequence: u32,
    pub final_d8_rise_sample: Option<u64>,
    pub final_d8_fall_sample: Option<u64>,
    pub shutdown_unadmitted_int1_edges: [u64; SENSOR_COUNT],
    pub shutdown_transactions: u64,
    pub shutdown_scl_rising_edges: u64,
    pub terminal_marker_rise_sample: Option<u64>,
    pub terminal_marker_fall_sample: Option<u64>,
    pub terminal_marker_width_samples: Option<u64>,
    pub terminal_idle_samples: u64,
    pub host_response_time_ns: u64,
    pub deterministic_quiesce_violations: u64,
    pub unexplained_events: u64,
}

/// Stable semantic failures derived from the complete raw waveform.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Gate0TraceViolationCode {
    CapturePlanMismatch,
    CaptureTooShort,
    BootAnchorMissing,
    BootAnchorOrder,
    BootAnchorWidth,
    ActivityBeforeBootAnchor,
    StartupTransactionMismatch,
    StartupIncomplete,
    StartupResetPollLimit,
    StartupPrematureAcquisition,
    StartupPrimingTimeout,
    StartupPrimingIncomplete,
    StartupKnownQuietInt1,
    ReservedLogicBitSet,
    AmbiguousBusTransition,
    I2cProtocol,
    I2cSequence,
    I2cShape,
    ReadWithoutInt1,
    ReadyBeforeFinalStatus,
    StatusNeverReady,
    DuplicateSensorEdge,
    NextEdgeCrossing,
    D9Missing,
    D9MarkerOrder,
    D8MarkerOrder,
    D8D9Overlap,
    MarkerBusTransitionOverlap,
    MarkerWidth,
    MissingSelectedRead,
    StaleOrWrongPayload,
    SessionFrameNotClean,
    SessionFrameIdentityMismatch,
    SessionSampleTimeMismatch,
    MissingAnalyzerSnapshot,
    ExtraAnalyzerSnapshot,
    IncompleteReadAtEnd,
    PendingEdgeAtEnd,
    UnselectedCacheAtEnd,
    MissingSixWayCollision,
    PollAverageAtLeastFour,
    OdrOutOfProvisionalBand,
    SnapshotSpreadExceeded,
    SelectedSampleTooOld,
    DecimationOutsideGrid,
    AccountingMismatch,
    FinalSnapshotMismatch,
    UnmatchedPreCutEdge,
    AcquisitionAfterFinalCut,
    ShutdownTransactionMismatch,
    ShutdownSuffixIncomplete,
    ShutdownSclCount,
    TerminalPremarkerIdleTooShort,
    TerminalMarkerMissing,
    TerminalMarkerShape,
    TerminalMarkerWidth,
    TerminalMarkerDeadline,
    TerminalIdleTooShort,
    PostMarkerActivity,
}

impl Gate0TraceViolationCode {
    const fn is_deterministic_quiesce_violation(self) -> bool {
        matches!(
            self,
            Self::FinalSnapshotMismatch
                | Self::UnmatchedPreCutEdge
                | Self::AcquisitionAfterFinalCut
                | Self::ShutdownTransactionMismatch
                | Self::ShutdownSuffixIncomplete
                | Self::ShutdownSclCount
                | Self::TerminalPremarkerIdleTooShort
                | Self::TerminalMarkerMissing
                | Self::TerminalMarkerShape
                | Self::TerminalMarkerWidth
                | Self::TerminalMarkerDeadline
                | Self::TerminalIdleTooShort
                | Self::PostMarkerActivity
        )
    }
}

/// One bounded location for a semantic violation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct Gate0TraceViolation {
    pub code: Gate0TraceViolationCode,
    pub sample_ordinal: u64,
    pub sensor_index: Option<u8>,
}

/// One deterministic extremum or collision witness.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct Gate0TracePointWitness {
    pub sample_ordinal: u64,
    pub sensor_index: Option<u8>,
    pub value: u64,
}

/// Bounded witnesses; the full metrics, not these samples, decide cleanliness.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct Gate0TraceWitnesses {
    pub boot_anchor_end: Option<Gate0TracePointWitness>,
    pub first_linked_snapshot: Option<Gate0TracePointWitness>,
    pub worst_read_latency: Option<Gate0TracePointWitness>,
    pub worst_status_polls: Option<Gate0TracePointWitness>,
    pub six_way_collision: Option<Gate0TracePointWitness>,
    pub violations: Vec<Gate0TraceViolation>,
    pub violation_count: u64,
    pub violation_overflow: u64,
}

/// Repository-derived evidence consumed by the Gate 0A scorer.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Gate0TraceEvidence {
    schema: &'static str,
    manifest_sha256: [u8; 32],
    session_sha256: [u8; 32],
    raw_capture_sha256: [u8; 32],
    sidecar_sha256: [u8; 32],
    settings: Gate0CaptureSettings,
    metrics: Gate0TraceMetrics,
    witnesses: Gate0TraceWitnesses,
}

impl Gate0TraceEvidence {
    /// True only when complete raw reduction produced no semantic violation.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.witnesses.violation_count == 0
    }

    /// True only when the session-selected final boundary, fixed shutdown,
    /// simultaneous marker, and finite terminal silence all passed.
    #[must_use]
    pub fn deterministic_quiesce(&self) -> bool {
        self.metrics.deterministic_quiesce_violations == 0
            && self.metrics.final_d8_rise_sample.is_some()
            && self.metrics.final_d8_fall_sample.is_some()
            && self.metrics.shutdown_transactions == SHUTDOWN_TRANSACTION_COUNT as u64
            && self.metrics.shutdown_scl_rising_edges == SHUTDOWN_SCL_RISING_EDGES
            && self.metrics.terminal_marker_rise_sample.is_some()
            && self.metrics.terminal_marker_fall_sample.is_some()
            && self
                .metrics
                .terminal_marker_width_samples
                .is_some_and(|width| {
                    (TERMINAL_MARKER_MIN_TICKS..=TERMINAL_MARKER_MAX_TICKS).contains(&width)
                })
            && self.metrics.terminal_idle_samples >= TERMINAL_IDLE_MINIMUM_SAMPLES
            && self.metrics.host_response_time_ns <= 750_000_000
    }

    /// Prove this evidence used the exact manifest and session being scored.
    #[must_use]
    pub fn binds(&self, manifest_sha256: &[u8; 32], session_sha256: &[u8; 32]) -> bool {
        &self.manifest_sha256 == manifest_sha256 && &self.session_sha256 == session_sha256
    }

    #[must_use]
    pub const fn raw_capture_sha256(&self) -> &[u8; 32] {
        &self.raw_capture_sha256
    }

    #[must_use]
    pub const fn sidecar_sha256(&self) -> &[u8; 32] {
        &self.sidecar_sha256
    }

    #[must_use]
    pub const fn settings(&self) -> &Gate0CaptureSettings {
        &self.settings
    }

    #[must_use]
    pub const fn metrics(&self) -> &Gate0TraceMetrics {
        &self.metrics
    }

    /// Maximum analyzer-derived spread rounded up to whole microseconds.
    #[must_use]
    pub const fn max_snapshot_spread_us(&self) -> u64 {
        self.metrics
            .max_snapshot_spread_ticks
            .div_ceil(TICKS_PER_MICROSECOND)
    }

    #[must_use]
    pub const fn witnesses(&self) -> &Gate0TraceWitnesses {
        &self.witnesses
    }
}

/// Structural failures that prevent a trustworthy raw reduction.
#[derive(Debug)]
pub enum Gate0TraceError {
    Io(io::Error),
    Manifest(ManifestError),
    Session(SessionError),
    TerminalSession(Gate0aTerminalError),
    TerminalBindingMismatch,
    SidecarTooLarge,
    InvalidSidecar(String),
    UnsupportedSidecarSchema(String),
    InvalidSidecarField(&'static str),
    InvalidArtifactPath(&'static str),
    ArtifactHashMismatch(&'static str),
    ArtifactSizeMismatch(&'static str),
    CaptureLogRejected(String),
    EmptyRawCapture,
    OddRawCaptureLength,
    RawCaptureHashMismatch,
}

impl fmt::Display for Gate0TraceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => error.fmt(formatter),
            Self::Manifest(error) => error.fmt(formatter),
            Self::Session(error) => error.fmt(formatter),
            Self::TerminalSession(error) => error.fmt(formatter),
            Self::TerminalBindingMismatch => formatter
                .write_str("Gate 0A terminal records do not bind the one-hand manifest identity"),
            Self::SidecarTooLarge => write!(
                formatter,
                "Gate 0A capture sidecar exceeds {MAX_GATE0_CAPTURE_SIDECAR_BYTES} bytes"
            ),
            Self::InvalidSidecar(message) => {
                write!(formatter, "invalid Gate 0A sidecar: {message}")
            }
            Self::UnsupportedSidecarSchema(schema) => {
                write!(formatter, "unsupported Gate 0A sidecar schema {schema:?}")
            }
            Self::InvalidSidecarField(field) => {
                write!(formatter, "invalid Gate 0A sidecar field {field}")
            }
            Self::InvalidArtifactPath(field) => {
                write!(
                    formatter,
                    "Gate 0A sidecar field {field} is not a safe relative path"
                )
            }
            Self::ArtifactHashMismatch(artifact) => {
                write!(
                    formatter,
                    "Gate 0A {artifact} hash does not match its sidecar binding"
                )
            }
            Self::ArtifactSizeMismatch(artifact) => {
                write!(
                    formatter,
                    "Gate 0A {artifact} size does not match its sidecar binding"
                )
            }
            Self::CaptureLogRejected(reason) => {
                write!(formatter, "Gate 0A capture log was rejected: {reason}")
            }
            Self::EmptyRawCapture => formatter.write_str("Gate 0A raw capture is empty"),
            Self::OddRawCaptureLength => {
                formatter.write_str("Gate 0A raw capture ends with an incomplete 16-bit word")
            }
            Self::RawCaptureHashMismatch => {
                formatter.write_str("Gate 0A raw capture hash does not match its sidecar binding")
            }
        }
    }
}

impl Error for Gate0TraceError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Manifest(error) => Some(error),
            Self::Session(error) => Some(error),
            Self::TerminalSession(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for Gate0TraceError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<ManifestError> for Gate0TraceError {
    fn from(error: ManifestError) -> Self {
        Self::Manifest(error)
    }
}

impl From<SessionError> for Gate0TraceError {
    fn from(error: SessionError) -> Self {
        Self::Session(error)
    }
}

impl From<Gate0aTerminalError> for Gate0TraceError {
    fn from(error: Gate0aTerminalError) -> Self {
        Self::TerminalSession(error)
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureSidecar {
    schema: String,
    raw_capture_sha256: String,
    raw_capture_bytes: u64,
    word_bytes: u8,
    byte_order: String,
    sample_rate_hz: u64,
    channel_map: Gate0ChannelMap,
    voltage_threshold_low_mv: u32,
    voltage_threshold_high_mv: u32,
    requested_samples: u64,
    capture_started_unix_ms: u64,
    capture_finished_unix_ms: u64,
    capture_binary: BoundArtifact,
    capture_log: BoundArtifact,
    device_firmware_bundle: BoundArtifact,
    driver_base_commit: String,
    sigrok_cli_base_commit: String,
    driver_patch_sha256: String,
    device_model: String,
    capture_exit_status: i32,
    command: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BoundArtifact {
    path: String,
    sha256: String,
    bytes: u64,
}

#[derive(Clone, Copy, Debug, Default)]
struct RunningMoments {
    count: u64,
    mean: f64,
    m2: f64,
}

impl RunningMoments {
    fn push(&mut self, value: u64) {
        self.count += 1;
        let delta = value as f64 - self.mean;
        self.mean += delta / self.count as f64;
        self.m2 += delta * (value as f64 - self.mean);
    }

    fn rms(self) -> Option<u64> {
        (self.count > 0).then(|| (self.m2 / self.count as f64).sqrt().round() as u64)
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct EdgeStats {
    first_tick: Option<u64>,
    last_tick: Option<u64>,
    intervals: RunningMoments,
}

#[derive(Clone, Copy, Debug, Default)]
enum BootAnchorState {
    #[default]
    AwaitD8Rise,
    D8High {
        rise_tick: u64,
    },
    D8LowGap {
        fall_tick: u64,
    },
    D9High {
        rise_tick: u64,
    },
    LowGuard {
        fall_tick: u64,
    },
    Complete,
    Failed,
}

#[derive(Clone, Debug)]
struct I2cTransaction {
    begin_tick: u64,
    end_tick: u64,
    address: u8,
    write: [u8; MAX_I2C_BYTES],
    write_len: usize,
    read: [u8; MAX_I2C_BYTES],
    read_len: usize,
    scl_pulses: u16,
}

#[derive(Clone, Debug, Default)]
struct I2cDecoder {
    active: bool,
    begin_tick: u64,
    address: Option<u8>,
    direction_read: bool,
    expecting_address: bool,
    bit_index: u8,
    byte: u8,
    write: [u8; MAX_I2C_BYTES],
    write_len: usize,
    read: [u8; MAX_I2C_BYTES],
    read_len: usize,
    previous_read_ack_high: bool,
    termination_clock_pending: bool,
    scl_pulses: u16,
    last_scl_rise: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum I2cFault {
    AmbiguousTransition,
    Protocol,
    TooManyBytes,
}

impl I2cDecoder {
    fn observe(
        &mut self,
        previous: u16,
        current: u16,
        tick: u64,
        metrics: &mut Gate0TraceMetrics,
    ) -> Result<Option<I2cTransaction>, I2cFault> {
        let sda_changed = (previous ^ current) & SDA_MASK != 0;
        let scl_changed = (previous ^ current) & SCL_MASK != 0;
        if sda_changed && scl_changed {
            return Err(I2cFault::AmbiguousTransition);
        }

        let scl_high = current & SCL_MASK != 0;
        if sda_changed && scl_high {
            if current & SDA_MASK == 0 {
                self.start(tick)?;
                return Ok(None);
            }
            return self.stop(tick);
        }

        if scl_changed && self.active && !scl_high && self.termination_clock_pending {
            return Err(I2cFault::Protocol);
        }

        if scl_changed && scl_high && self.active {
            metrics.scl_rising_edges += 1;
            if let Some(previous_tick) = self.last_scl_rise {
                let period = tick - previous_tick;
                metrics.minimum_scl_period_ticks = Some(
                    metrics
                        .minimum_scl_period_ticks
                        .map_or(period, |old| old.min(period)),
                );
                metrics.maximum_scl_period_ticks = Some(
                    metrics
                        .maximum_scl_period_ticks
                        .map_or(period, |old| old.max(period)),
                );
            }
            self.last_scl_rise = Some(tick);
            if self.direction_read && self.previous_read_ack_high {
                // A master prepares STOP or repeated START by raising SCL once more
                // after NACKing the final read byte.  It must issue that condition
                // before lowering SCL again; otherwise the stream is malformed.
                self.termination_clock_pending = true;
            } else {
                self.clock(current & SDA_MASK != 0)?;
            }
        }
        Ok(None)
    }

    fn start(&mut self, tick: u64) -> Result<(), I2cFault> {
        if self.active {
            self.discard_condition_setup_bit(true)?;
            if self.bit_index != 0 || self.expecting_address {
                return Err(I2cFault::Protocol);
            }
            if self.direction_read && !self.previous_read_ack_high {
                return Err(I2cFault::Protocol);
            }
        } else {
            *self = Self::default();
            self.active = true;
            self.begin_tick = tick;
        }
        self.expecting_address = true;
        self.direction_read = false;
        self.bit_index = 0;
        self.byte = 0;
        self.previous_read_ack_high = false;
        self.termination_clock_pending = false;
        self.last_scl_rise = None;
        Ok(())
    }

    fn clock(&mut self, sda_high: bool) -> Result<(), I2cFault> {
        self.scl_pulses = self.scl_pulses.checked_add(1).ok_or(I2cFault::Protocol)?;
        if self.bit_index < 8 {
            self.byte = (self.byte << 1) | u8::from(sda_high);
            self.bit_index += 1;
            return Ok(());
        }

        if self.expecting_address {
            if sda_high {
                return Err(I2cFault::Protocol);
            }
            let address = self.byte >> 1;
            if let Some(existing) = self.address {
                if existing != address {
                    return Err(I2cFault::Protocol);
                }
            } else {
                self.address = Some(address);
            }
            self.direction_read = self.byte & 1 != 0;
            self.expecting_address = false;
        } else if self.direction_read {
            if self.previous_read_ack_high {
                return Err(I2cFault::Protocol);
            }
            if self.read_len == MAX_I2C_BYTES {
                return Err(I2cFault::TooManyBytes);
            }
            self.read[self.read_len] = self.byte;
            self.read_len += 1;
            self.previous_read_ack_high = sda_high;
        } else {
            if sda_high {
                return Err(I2cFault::Protocol);
            }
            if self.write_len == MAX_I2C_BYTES {
                return Err(I2cFault::TooManyBytes);
            }
            self.write[self.write_len] = self.byte;
            self.write_len += 1;
        }
        self.bit_index = 0;
        self.byte = 0;
        Ok(())
    }

    fn stop(&mut self, tick: u64) -> Result<Option<I2cTransaction>, I2cFault> {
        self.discard_condition_setup_bit(false)?;
        if !self.active
            || self.bit_index != 0
            || self.expecting_address
            || self.address.is_none()
            || (self.direction_read && !self.previous_read_ack_high)
        {
            return Err(I2cFault::Protocol);
        }
        let transaction = I2cTransaction {
            begin_tick: self.begin_tick,
            end_tick: tick,
            address: self.address.expect("checked"),
            write: self.write,
            write_len: self.write_len,
            read: self.read,
            read_len: self.read_len,
            scl_pulses: self.scl_pulses,
        };
        *self = Self::default();
        Ok(Some(transaction))
    }

    // While SCL is low, SDA is placed at the level needed for START/STOP, then
    // SCL rises before SDA changes.  That rise looks like the first data bit
    // until the following SDA edge identifies the bus condition.  Keep the
    // decoder strict while allowing that one provisional physical clock.
    fn discard_condition_setup_bit(&mut self, expected_high: bool) -> Result<(), I2cFault> {
        if self.termination_clock_pending {
            self.termination_clock_pending = false;
            return Ok(());
        }
        if self.bit_index == 1 && self.byte == u8::from(expected_high) {
            self.bit_index = 0;
            self.byte = 0;
            self.scl_pulses = self.scl_pulses.checked_sub(1).ok_or(I2cFault::Protocol)?;
            return Ok(());
        }
        if self.bit_index == 0 {
            return Ok(());
        }
        Err(I2cFault::Protocol)
    }
}

#[derive(Clone, Copy, Debug)]
struct PendingEdge {
    tick: u64,
}

#[derive(Clone, Debug)]
struct ReadWork {
    sensor_index: usize,
    edge_tick: u64,
    status_count: u8,
}

#[derive(Clone, Debug, Default)]
enum ReadState {
    #[default]
    Idle,
    MuxReadback(ReadWork),
    Status(ReadWork),
    Motion(ReadWork),
    AwaitingD9(CompletedRead),
}

#[derive(Clone, Debug)]
struct CompletedRead {
    sensor_index: usize,
    edge_tick: u64,
    completed_tick: u64,
    motion: [u8; 12],
}

#[derive(Clone, Copy, Debug, Default)]
enum StartupStep {
    #[default]
    MuxWrite,
    MuxReadback,
    ResetWrite,
    ResetPoll,
    Identity,
    ProfileWrite(usize),
    ProfileReadback(usize),
    ArmMuxWrite,
    ArmMuxReadback,
    ArmEnableWrite,
    ArmEnableReadback,
    Failed,
}

#[derive(Clone, Copy, Debug, Default)]
struct StartupState {
    sensor_index: usize,
    step: StartupStep,
    reset_polls: u8,
    earliest_transaction_tick: u64,
    admitted_sensor_mask: u8,
    quiet_sensor_mask: u8,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum QuiescePhase {
    #[default]
    Initializing,
    Acquiring,
    FinalSnapshotHigh,
    Shutdown,
    MarkerHigh,
    Quiescent,
}

struct SessionCursor<'a> {
    reader: SessionReader<'a>,
    next: Option<SensorFrame>,
    exhausted: bool,
}

impl<'a> SessionCursor<'a> {
    fn new(session: &'a FinalizedSession) -> Self {
        Self {
            reader: session.reader(),
            next: None,
            exhausted: false,
        }
    }

    fn fill(&mut self) -> Result<(), SessionError> {
        while self.next.is_none() && !self.exhausted {
            let Some(record) = self.reader.next_record()? else {
                self.exhausted = true;
                break;
            };
            if record.kind != SessionRecordKind::Decoded {
                continue;
            }
            if let Ok(frame) = record.decode() {
                self.next = Some(frame);
            }
        }
        Ok(())
    }

    fn frame_for_sequence(
        &mut self,
        sequence: u32,
    ) -> Result<(Option<SensorFrame>, u64), SessionError> {
        let mut missing_analyzer_snapshots = 0;
        loop {
            self.fill()?;
            let Some(frame) = self.next else {
                return Ok((None, missing_analyzer_snapshots));
            };
            if frame.sequence < sequence {
                missing_analyzer_snapshots += 1;
                self.next = None;
                continue;
            }
            if frame.sequence == sequence {
                self.next = None;
                return Ok((Some(frame), missing_analyzer_snapshots));
            }
            return Ok((None, missing_analyzer_snapshots));
        }
    }

    fn remaining_frames(&mut self) -> Result<u64, SessionError> {
        let mut remaining = 0;
        loop {
            self.fill()?;
            if self.next.take().is_some() {
                remaining += 1;
            } else {
                return Ok(remaining);
            }
        }
    }
}

struct TraceReducer {
    target_identity: FrameIdentity,
    final_sequence: u32,
    quiesce_phase: QuiescePhase,
    startup: StartupState,
    next_shutdown_transaction: usize,
    premarker_idle_start_tick: Option<u64>,
    metrics: Gate0TraceMetrics,
    witnesses: Gate0TraceWitnesses,
    edge_stats: [EdgeStats; SENSOR_COUNT],
    pending_edges: [Option<PendingEdge>; SENSOR_COUNT],
    pending_count: u8,
    read_state: ReadState,
    cache: [Option<CompletedRead>; SENSOR_COUNT],
    d8_rise_tick: Option<u64>,
    d9_rise_tick: Option<u64>,
    last_d8_rise_tick: Option<u64>,
}

impl TraceReducer {
    fn new(
        target_identity: FrameIdentity,
        final_sequence: u32,
        host_response_time_ns: u64,
    ) -> Self {
        let metrics = Gate0TraceMetrics {
            final_sequence,
            host_response_time_ns,
            ..Gate0TraceMetrics::default()
        };
        Self {
            target_identity,
            final_sequence,
            quiesce_phase: QuiescePhase::Initializing,
            startup: StartupState::default(),
            next_shutdown_transaction: 0,
            premarker_idle_start_tick: None,
            metrics,
            witnesses: Gate0TraceWitnesses::default(),
            edge_stats: [EdgeStats::default(); SENSOR_COUNT],
            pending_edges: [None; SENSOR_COUNT],
            pending_count: 0,
            read_state: ReadState::Idle,
            cache: std::array::from_fn(|_| None),
            d8_rise_tick: None,
            d9_rise_tick: None,
            last_d8_rise_tick: None,
        }
    }

    fn violate(&mut self, code: Gate0TraceViolationCode, tick: u64, sensor_index: Option<usize>) {
        if code.is_deterministic_quiesce_violation() {
            self.metrics.deterministic_quiesce_violations = self
                .metrics
                .deterministic_quiesce_violations
                .saturating_add(1);
        }
        self.witnesses.violation_count += 1;
        if self.witnesses.violations.len() < MAX_GATE0_TRACE_VIOLATION_WITNESSES {
            self.witnesses.violations.push(Gate0TraceViolation {
                code,
                sample_ordinal: tick,
                sensor_index: sensor_index.and_then(|index| u8::try_from(index).ok()),
            });
        } else {
            self.witnesses.violation_overflow += 1;
        }
    }

    fn int1_rise(&mut self, tick: u64, sensor_index: usize) {
        self.metrics.int1_edges[sensor_index] += 1;
        let previous_tick = self.edge_stats[sensor_index].last_tick;
        if let Some(previous_tick) = previous_tick {
            if tick <= previous_tick {
                self.violate(
                    Gate0TraceViolationCode::DuplicateSensorEdge,
                    tick,
                    Some(sensor_index),
                );
            } else {
                let interval_ticks = tick - previous_tick;
                self.edge_stats[sensor_index].intervals.push(interval_ticks);
                if !odr_in_provisional_band(interval_ticks) {
                    self.violate(
                        Gate0TraceViolationCode::OdrOutOfProvisionalBand,
                        tick,
                        Some(sensor_index),
                    );
                }
            }
        } else {
            self.edge_stats[sensor_index].first_tick = Some(tick);
        }
        self.edge_stats[sensor_index].last_tick = Some(tick);

        if self.pending_edges[sensor_index].is_some() {
            self.metrics.next_edge_crossings += 1;
            self.violate(
                Gate0TraceViolationCode::NextEdgeCrossing,
                tick,
                Some(sensor_index),
            );
            return;
        }
        self.pending_edges[sensor_index] = Some(PendingEdge { tick });
        self.pending_count = self.pending_count.saturating_add(1);
        self.metrics.max_pending_reads = self.metrics.max_pending_reads.max(self.pending_count);
        if self.pending_count == SENSOR_COUNT as u8 && self.witnesses.six_way_collision.is_none() {
            self.witnesses.six_way_collision = Some(Gate0TracePointWitness {
                sample_ordinal: tick,
                sensor_index: None,
                value: u64::from(self.pending_count),
            });
        }
    }

    fn shutdown_int1_rise(&mut self, sensor_index: usize) {
        self.metrics.shutdown_unadmitted_int1_edges[sensor_index] =
            self.metrics.shutdown_unadmitted_int1_edges[sensor_index].saturating_add(1);
    }

    fn is_final_d8_rise(&self, previous: u16, word: u16) -> bool {
        self.quiesce_phase == QuiescePhase::Acquiring
            && previous & D8_MASK == 0
            && word & D8_MASK != 0
            && self.metrics.snapshots == u64::from(self.final_sequence)
    }

    fn final_cut_has_active_i2c(&mut self, tick: u64) {
        self.violate(
            Gate0TraceViolationCode::AcquisitionAfterFinalCut,
            tick,
            None,
        );
    }

    fn classify_i2c_fault(&mut self, tick: u64) {
        match self.quiesce_phase {
            QuiescePhase::Initializing => {
                self.startup.step = StartupStep::Failed;
                self.violate(
                    Gate0TraceViolationCode::StartupTransactionMismatch,
                    tick,
                    None,
                );
            }
            QuiescePhase::FinalSnapshotHigh => self.final_cut_has_active_i2c(tick),
            QuiescePhase::Shutdown => self.violate(
                Gate0TraceViolationCode::ShutdownTransactionMismatch,
                tick,
                None,
            ),
            _ => {}
        }
    }

    /// Consume D8/D9 terminal transitions before the ordinary marker paths.
    fn terminal_marker_transition(
        &mut self,
        previous: u16,
        word: u16,
        changed: u16,
        tick: u64,
    ) -> bool {
        match self.quiesce_phase {
            QuiescePhase::Shutdown
                if self.next_shutdown_transaction == SHUTDOWN_TRANSACTION_COUNT
                    && changed & (D8_MASK | D9_MASK) != 0 =>
            {
                if previous == TERMINAL_IDLE_WORD
                    && word == TERMINAL_MARKER_WORD
                    && changed == D8_MASK | D9_MASK
                {
                    let idle_ticks = self
                        .premarker_idle_start_tick
                        .map_or(0, |end_tick| tick.saturating_sub(end_tick));
                    if idle_ticks < TERMINAL_PREMARKER_IDLE_TICKS {
                        self.violate(
                            Gate0TraceViolationCode::TerminalPremarkerIdleTooShort,
                            tick,
                            None,
                        );
                    }
                    self.metrics.terminal_marker_rise_sample = Some(tick);
                    self.quiesce_phase = QuiescePhase::MarkerHigh;
                } else {
                    self.violate(Gate0TraceViolationCode::TerminalMarkerShape, tick, None);
                }
                true
            }
            QuiescePhase::MarkerHigh => {
                if previous == TERMINAL_MARKER_WORD
                    && word == TERMINAL_IDLE_WORD
                    && changed == D8_MASK | D9_MASK
                {
                    let rise_tick = self
                        .metrics
                        .terminal_marker_rise_sample
                        .expect("marker-high phase has a rise");
                    let width = tick.saturating_sub(rise_tick);
                    self.metrics.terminal_marker_fall_sample = Some(tick);
                    self.metrics.terminal_marker_width_samples = Some(width);
                    if !(TERMINAL_MARKER_MIN_TICKS..=TERMINAL_MARKER_MAX_TICKS).contains(&width) {
                        self.violate(Gate0TraceViolationCode::TerminalMarkerWidth, tick, None);
                    }
                    if tick > TERMINAL_MARKER_FALL_DEADLINE {
                        self.violate(Gate0TraceViolationCode::TerminalMarkerDeadline, tick, None);
                    }
                    self.quiesce_phase = QuiescePhase::Quiescent;
                } else {
                    self.violate(Gate0TraceViolationCode::TerminalMarkerShape, tick, None);
                }
                true
            }
            QuiescePhase::Quiescent => {
                self.violate(Gate0TraceViolationCode::PostMarkerActivity, tick, None);
                true
            }
            QuiescePhase::Initializing
            | QuiescePhase::Acquiring
            | QuiescePhase::FinalSnapshotHigh => false,
            QuiescePhase::Shutdown => false,
        }
    }

    fn observe_premarker_word(&mut self, word: u16, tick: u64) {
        if self.quiesce_phase == QuiescePhase::Shutdown
            && self.next_shutdown_transaction == SHUTDOWN_TRANSACTION_COUNT
        {
            self.premarker_idle_start_tick = (word == TERMINAL_IDLE_WORD).then_some(tick);
        }
    }

    fn transaction(&mut self, transaction: I2cTransaction) {
        self.metrics.decoded_i2c_transactions += 1;
        match self.quiesce_phase {
            QuiescePhase::Initializing => self.startup_transaction(transaction),
            QuiescePhase::Acquiring => self.acquisition_transaction(transaction),
            QuiescePhase::FinalSnapshotHigh => {
                self.violate(
                    Gate0TraceViolationCode::AcquisitionAfterFinalCut,
                    transaction.end_tick,
                    None,
                );
            }
            QuiescePhase::Shutdown => self.shutdown_transaction(transaction),
            QuiescePhase::MarkerHigh | QuiescePhase::Quiescent => {
                self.violate(
                    Gate0TraceViolationCode::PostMarkerActivity,
                    transaction.end_tick,
                    None,
                );
            }
        }
    }

    fn startup_i2c_start(&mut self) {
        if self.quiesce_phase == QuiescePhase::Initializing
            && matches!(self.startup.step, StartupStep::ArmEnableWrite)
        {
            // Callbacks are installed before arming. Count even an INT1 that
            // rises during the enable write, before its STOP or readback.
            self.startup.admitted_sensor_mask |= 1_u8 << self.startup.sensor_index;
            self.startup.quiet_sensor_mask &= !(1_u8 << self.startup.sensor_index);
        }
    }

    fn startup_transaction(&mut self, transaction: I2cTransaction) {
        self.metrics.startup_transactions += 1;
        if matches!(self.startup.step, StartupStep::Failed) {
            return;
        }
        let sensor_index = self.startup.sensor_index;
        let mux_mask = 1_u8 << sensor_index;
        let on_time = transaction.begin_tick >= self.startup.earliest_transaction_tick;
        let valid = match self.startup.step {
            StartupStep::MuxWrite | StartupStep::ArmMuxWrite => {
                transfer_matches(&transaction, MUX_ADDRESS, &[mux_mask], &[], 18)
            }
            StartupStep::MuxReadback | StartupStep::ArmMuxReadback => {
                transfer_matches(&transaction, MUX_ADDRESS, &[], &[mux_mask], 18)
            }
            StartupStep::ResetWrite => transfer_matches(
                &transaction,
                IMU_ADDRESS,
                &[RESET_REGISTER, RESET_MASK],
                &[],
                27,
            ),
            StartupStep::ResetPoll => {
                transaction.address == IMU_ADDRESS
                    && transaction.write_len == 1
                    && transaction.write[0] == RESET_REGISTER
                    && transaction.read_len == 1
                    && transaction.scl_pulses == 36
            }
            StartupStep::Identity => transfer_matches(
                &transaction,
                IMU_ADDRESS,
                &[WHO_AM_I_REGISTER],
                &[WHO_AM_I_VALUE],
                36,
            ),
            StartupStep::ProfileWrite(index) => {
                let (register, value) = STARTUP_PROFILE[index];
                transfer_matches(&transaction, IMU_ADDRESS, &[register, value], &[], 27)
            }
            StartupStep::ProfileReadback(index) => {
                let (register, value) = STARTUP_PROFILE[index];
                transfer_matches(&transaction, IMU_ADDRESS, &[register], &[value], 36)
            }
            StartupStep::ArmEnableWrite => {
                transfer_matches(&transaction, IMU_ADDRESS, &[0x0d, 0x01], &[], 27)
            }
            StartupStep::ArmEnableReadback => {
                transfer_matches(&transaction, IMU_ADDRESS, &[0x0d], &[0x01], 36)
            }
            StartupStep::Failed => unreachable!("failed initialization returned above"),
        };
        if !valid || !on_time {
            self.startup.step = StartupStep::Failed;
            self.violate(
                Gate0TraceViolationCode::StartupTransactionMismatch,
                transaction.end_tick,
                Some(sensor_index),
            );
            return;
        }

        self.startup.earliest_transaction_tick = 0;
        self.startup.step = match self.startup.step {
            StartupStep::MuxWrite => {
                self.startup.earliest_transaction_tick =
                    transaction.end_tick.saturating_add(MUX_SETTLE_TICKS);
                StartupStep::MuxReadback
            }
            StartupStep::MuxReadback => StartupStep::ResetWrite,
            StartupStep::ResetWrite => StartupStep::ResetPoll,
            StartupStep::ResetPoll => {
                self.startup.reset_polls += 1;
                if transaction.read[0] & RESET_MASK == 0 {
                    StartupStep::Identity
                } else if self.startup.reset_polls == RESET_POLL_LIMIT {
                    self.violate(
                        Gate0TraceViolationCode::StartupResetPollLimit,
                        transaction.end_tick,
                        Some(sensor_index),
                    );
                    StartupStep::Failed
                } else {
                    self.startup.earliest_transaction_tick =
                        transaction.end_tick.saturating_add(RESET_POLL_DELAY_TICKS);
                    StartupStep::ResetPoll
                }
            }
            StartupStep::Identity => StartupStep::ProfileWrite(0),
            StartupStep::ProfileWrite(index) => StartupStep::ProfileReadback(index),
            StartupStep::ProfileReadback(index) if index + 1 < STARTUP_PROFILE.len() => {
                StartupStep::ProfileWrite(index + 1)
            }
            StartupStep::ProfileReadback(_) => {
                self.startup.quiet_sensor_mask |= 1_u8 << self.startup.sensor_index;
                self.startup.sensor_index += 1;
                self.startup.reset_polls = 0;
                if self.startup.sensor_index == SENSOR_COUNT {
                    self.startup.sensor_index = 0;
                    StartupStep::ArmMuxWrite
                } else {
                    StartupStep::MuxWrite
                }
            }
            StartupStep::ArmMuxWrite => {
                self.startup.earliest_transaction_tick =
                    transaction.end_tick.saturating_add(MUX_SETTLE_TICKS);
                StartupStep::ArmMuxReadback
            }
            StartupStep::ArmMuxReadback => StartupStep::ArmEnableWrite,
            StartupStep::ArmEnableWrite => StartupStep::ArmEnableReadback,
            StartupStep::ArmEnableReadback => {
                self.startup.sensor_index += 1;
                if self.startup.sensor_index == SENSOR_COUNT {
                    self.metrics.startup_end_sample = Some(transaction.end_tick);
                    self.quiesce_phase = QuiescePhase::Acquiring;
                }
                StartupStep::ArmMuxWrite
            }
            StartupStep::Failed => unreachable!("failed initialization returned above"),
        };
    }

    fn acquisition_transaction(&mut self, transaction: I2cTransaction) {
        if self.d9_rise_tick.is_some() {
            self.violate(
                Gate0TraceViolationCode::I2cSequence,
                transaction.end_tick,
                None,
            );
        }
        let mut read_state = std::mem::take(&mut self.read_state);
        if matches!(read_state, ReadState::AwaitingD9(_)) {
            self.violate(
                Gate0TraceViolationCode::D9Missing,
                transaction.end_tick,
                None,
            );
            read_state = ReadState::Idle;
        }

        let write = &transaction.write[..transaction.write_len];
        let read = &transaction.read[..transaction.read_len];
        match read_state {
            ReadState::Idle => self.start_read(&transaction, write, read),
            ReadState::MuxReadback(work) => {
                if transaction.address != MUX_ADDRESS
                    || !write.is_empty()
                    || read != [1_u8 << work.sensor_index]
                    || transaction.scl_pulses != 18
                {
                    self.metrics.unexplained_events += 1;
                    self.violate(
                        Gate0TraceViolationCode::I2cShape,
                        transaction.end_tick,
                        Some(work.sensor_index),
                    );
                    return;
                }
                self.read_state = ReadState::Status(work);
            }
            ReadState::Status(mut work) => {
                if !is_status_transfer(&transaction, write, read) {
                    self.metrics.unexplained_events += 1;
                    self.violate(
                        Gate0TraceViolationCode::I2cSequence,
                        transaction.end_tick,
                        Some(work.sensor_index),
                    );
                    return;
                }
                work.status_count += 1;
                if status_ready(read[0]) {
                    self.read_state = ReadState::Motion(work);
                } else if usize::from(work.status_count) == MAX_STATUS_POLLS {
                    self.violate(
                        Gate0TraceViolationCode::StatusNeverReady,
                        transaction.end_tick,
                        Some(work.sensor_index),
                    );
                } else {
                    self.read_state = ReadState::Status(work);
                }
            }
            ReadState::Motion(mut work) => {
                if is_status_transfer(&transaction, write, read) {
                    self.violate(
                        Gate0TraceViolationCode::ReadyBeforeFinalStatus,
                        transaction.end_tick,
                        Some(work.sensor_index),
                    );
                    if usize::from(work.status_count) < MAX_STATUS_POLLS {
                        work.status_count += 1;
                    }
                    self.read_state = ReadState::Motion(work);
                    return;
                }
                if transaction.address != IMU_ADDRESS
                    || write != [MOTION_REGISTER]
                    || read.len() != 12
                    || transaction.scl_pulses != 135
                {
                    self.metrics.unexplained_events += 1;
                    self.violate(
                        Gate0TraceViolationCode::I2cShape,
                        transaction.end_tick,
                        Some(work.sensor_index),
                    );
                    return;
                }

                let status_polls = usize::from(work.status_count);
                self.metrics.completed_reads[work.sensor_index] += 1;
                self.metrics.total_status_polls += status_polls as u64;
                self.metrics.status_poll_histogram[status_polls - 1] += 1;
                let worst = self
                    .witnesses
                    .worst_status_polls
                    .map_or(0, |witness| witness.value);
                if status_polls as u64 > worst {
                    self.witnesses.worst_status_polls = Some(Gate0TracePointWitness {
                        sample_ordinal: transaction.end_tick,
                        sensor_index: Some(work.sensor_index as u8),
                        value: status_polls as u64,
                    });
                }
                let read_latency = transaction.end_tick.saturating_sub(work.edge_tick);
                if read_latency > self.metrics.max_read_latency_ticks {
                    self.metrics.max_read_latency_ticks = read_latency;
                    self.witnesses.worst_read_latency = Some(Gate0TracePointWitness {
                        sample_ordinal: transaction.end_tick,
                        sensor_index: Some(work.sensor_index as u8),
                        value: read_latency,
                    });
                }
                self.read_state = ReadState::AwaitingD9(CompletedRead {
                    sensor_index: work.sensor_index,
                    edge_tick: work.edge_tick,
                    completed_tick: transaction.end_tick,
                    motion: read.try_into().expect("length checked"),
                });
            }
            ReadState::AwaitingD9(_) => unreachable!("handled before transaction dispatch"),
        }
    }

    fn shutdown_transaction(&mut self, transaction: I2cTransaction) {
        self.metrics.shutdown_transactions = self.metrics.shutdown_transactions.saturating_add(1);
        self.metrics.shutdown_scl_rising_edges = self
            .metrics
            .shutdown_scl_rising_edges
            .saturating_add(u64::from(transaction.scl_pulses));

        let begins_after_final_d8 = self
            .metrics
            .final_d8_fall_sample
            .is_some_and(|fall| transaction.begin_tick > fall);
        if self.next_shutdown_transaction < SHUTDOWN_TRANSACTION_COUNT
            && begins_after_final_d8
            && shutdown_transaction_matches(self.next_shutdown_transaction, &transaction)
        {
            self.next_shutdown_transaction += 1;
            return;
        }

        self.violate(
            Gate0TraceViolationCode::ShutdownTransactionMismatch,
            transaction.end_tick,
            None,
        );
    }

    fn start_read(&mut self, transaction: &I2cTransaction, write: &[u8], read: &[u8]) {
        let valid_mux = transaction.address == MUX_ADDRESS
            && write.len() == 1
            && read.is_empty()
            && transaction.scl_pulses == 18
            && write[0].count_ones() == 1
            && write[0].trailing_zeros() < SENSOR_COUNT as u32;
        if !valid_mux {
            self.metrics.unexplained_events += 1;
            self.violate(
                Gate0TraceViolationCode::I2cShape,
                transaction.end_tick,
                None,
            );
            return;
        }
        let sensor_index = write[0].trailing_zeros() as usize;
        let edge_tick = if let Some(edge) = self.pending_edges[sensor_index] {
            edge.tick
        } else {
            self.violate(
                Gate0TraceViolationCode::ReadWithoutInt1,
                transaction.end_tick,
                Some(sensor_index),
            );
            transaction.begin_tick
        };
        self.read_state = ReadState::MuxReadback(ReadWork {
            sensor_index,
            edge_tick,
            status_count: 0,
        });
    }

    fn d9_edge(&mut self, tick: u64, asserted: bool) {
        if self.quiesce_phase == QuiescePhase::Initializing {
            self.violate(
                Gate0TraceViolationCode::StartupPrematureAcquisition,
                tick,
                None,
            );
            return;
        }
        if self.quiesce_phase != QuiescePhase::Acquiring {
            self.violate(
                Gate0TraceViolationCode::AcquisitionAfterFinalCut,
                tick,
                None,
            );
            return;
        }
        if asserted {
            if self.d9_rise_tick.is_some() {
                self.violate(Gate0TraceViolationCode::D9MarkerOrder, tick, None);
            }
            if self.d8_rise_tick.is_some() {
                self.violate(Gate0TraceViolationCode::D8D9Overlap, tick, None);
            }
            if !matches!(self.read_state, ReadState::AwaitingD9(_)) {
                self.violate(Gate0TraceViolationCode::D9MarkerOrder, tick, None);
            }
            self.d9_rise_tick = Some(tick);
            return;
        }

        let Some(rise_tick) = self.d9_rise_tick.take() else {
            self.violate(Gate0TraceViolationCode::D9MarkerOrder, tick, None);
            return;
        };
        if self.d8_rise_tick.is_some() {
            self.violate(Gate0TraceViolationCode::D8D9Overlap, tick, None);
        }
        self.check_normal_marker_width(rise_tick, tick);
        let state = std::mem::take(&mut self.read_state);
        if let ReadState::AwaitingD9(completed) = state {
            self.commit_read(completed, tick);
        } else {
            self.read_state = state;
        }
    }

    fn commit_read(&mut self, completed: CompletedRead, tick: u64) {
        let sensor_index = completed.sensor_index;
        self.metrics.committed_reads[sensor_index] += 1;
        let commit_delay = tick.saturating_sub(completed.completed_tick);
        self.metrics.max_i2c_to_d9_commit_ticks =
            self.metrics.max_i2c_to_d9_commit_ticks.max(commit_delay);
        if self.cache[sensor_index].replace(completed).is_some() {
            self.metrics.decimated_reads[sensor_index] += 1;
            if let Some(snapshot_tick) = self.last_d8_rise_tick {
                let phase = tick.saturating_sub(snapshot_tick);
                if phase >= MAX_SAMPLE_SPREAD_TICKS {
                    self.violate(
                        Gate0TraceViolationCode::DecimationOutsideGrid,
                        tick,
                        Some(sensor_index),
                    );
                }
                let bin = usize::try_from(phase / PHASE_BIN_WIDTH_TICKS)
                    .unwrap_or(usize::MAX)
                    .min(PHASE_BIN_COUNT - 1);
                self.metrics.phase_histogram[sensor_index][bin] += 1;
            } else {
                self.metrics.pre_snapshot_decimated_reads[sensor_index] += 1;
            }
        }
        if self.pending_edges[sensor_index].take().is_some() {
            self.pending_count = self.pending_count.saturating_sub(1);
        } else {
            self.violate(
                Gate0TraceViolationCode::AccountingMismatch,
                tick,
                Some(sensor_index),
            );
        }
        if self.metrics.first_clean_cache_sample.is_none() && self.cache.iter().all(Option::is_some)
        {
            self.metrics.first_clean_cache_sample = Some(tick);
            if self
                .metrics
                .startup_end_sample
                .is_some_and(|end| tick.saturating_sub(end) > SENSOR_PRIMING_MAX_TICKS)
            {
                self.violate(Gate0TraceViolationCode::StartupPrimingTimeout, tick, None);
            }
        }
    }

    fn d8_edge(&mut self, tick: u64, asserted: bool, frame: Option<SensorFrame>) {
        if self.quiesce_phase == QuiescePhase::Initializing {
            self.violate(
                Gate0TraceViolationCode::StartupPrematureAcquisition,
                tick,
                None,
            );
            return;
        }
        if !asserted {
            let Some(rise_tick) = self.d8_rise_tick.take() else {
                self.violate(Gate0TraceViolationCode::D8MarkerOrder, tick, None);
                return;
            };
            self.check_normal_marker_width(rise_tick, tick);
            if self.quiesce_phase == QuiescePhase::FinalSnapshotHigh {
                self.metrics.final_d8_fall_sample = Some(tick);
                self.quiesce_phase = QuiescePhase::Shutdown;
            } else if self.quiesce_phase != QuiescePhase::Acquiring {
                self.violate(
                    Gate0TraceViolationCode::AcquisitionAfterFinalCut,
                    tick,
                    None,
                );
            }
            return;
        }

        if self.quiesce_phase != QuiescePhase::Acquiring {
            self.violate(
                Gate0TraceViolationCode::AcquisitionAfterFinalCut,
                tick,
                None,
            );
            return;
        }

        if self.d8_rise_tick.is_some() {
            self.violate(Gate0TraceViolationCode::D8MarkerOrder, tick, None);
        }
        if self.d9_rise_tick.is_some() {
            self.violate(Gate0TraceViolationCode::D8D9Overlap, tick, None);
        }
        self.d8_rise_tick = Some(tick);
        let sequence = self.metrics.snapshots as u32;
        let final_snapshot = sequence == self.final_sequence;
        self.metrics.snapshots += 1;
        let violations_before = self.witnesses.violation_count;
        let clean_final_frame = frame.as_ref().is_some_and(|frame| {
            frame.sequence == self.final_sequence
                && frame.identity() == self.target_identity
                && frame.flags == 0
                && frame.valid_sensor_mask == ALL_SENSORS_VALID
        });

        if let Some(frame) = frame {
            self.metrics.linked_snapshots += 1;
            if frame.sequence != sequence {
                self.violate(Gate0TraceViolationCode::MissingAnalyzerSnapshot, tick, None);
            }
            if frame.identity() != self.target_identity {
                self.violate(
                    Gate0TraceViolationCode::SessionFrameIdentityMismatch,
                    tick,
                    None,
                );
            }
            if frame.flags != 0 || frame.valid_sensor_mask != ALL_SENSORS_VALID {
                self.violate(Gate0TraceViolationCode::SessionFrameNotClean, tick, None);
            }
            self.select_cache(tick, Some(&frame));
            if self.witnesses.first_linked_snapshot.is_none()
                && self.witnesses.violation_count == violations_before
            {
                self.witnesses.first_linked_snapshot = Some(Gate0TracePointWitness {
                    sample_ordinal: tick,
                    sensor_index: None,
                    value: u64::from(sequence),
                });
            }
        } else {
            if self.metrics.linked_snapshots > 0 {
                self.violate(Gate0TraceViolationCode::ExtraAnalyzerSnapshot, tick, None);
            }
            self.metrics.host_dropped_snapshots += 1;
            self.select_cache(tick, None);
        }
        self.last_d8_rise_tick = Some(tick);

        if final_snapshot {
            self.metrics.final_d8_rise_sample = Some(tick);
            self.quiesce_phase = QuiescePhase::FinalSnapshotHigh;
            if !clean_final_frame || self.witnesses.violation_count != violations_before {
                self.violate(Gate0TraceViolationCode::FinalSnapshotMismatch, tick, None);
            }
            if !matches!(self.read_state, ReadState::Idle) || self.d9_rise_tick.is_some() {
                self.violate(
                    Gate0TraceViolationCode::AcquisitionAfterFinalCut,
                    tick,
                    None,
                );
            }
            for sensor_index in 0..SENSOR_COUNT {
                if self.pending_edges[sensor_index].is_some() {
                    self.violate(
                        Gate0TraceViolationCode::UnmatchedPreCutEdge,
                        tick,
                        Some(sensor_index),
                    );
                }
            }
        }
    }

    fn select_cache(&mut self, tick: u64, frame: Option<&SensorFrame>) {
        let mut selected_ticks = [None; SENSOR_COUNT];
        for (sensor_index, selected_tick) in selected_ticks.iter_mut().enumerate() {
            let Some(selected) = self.cache[sensor_index].take() else {
                self.violate(
                    Gate0TraceViolationCode::MissingSelectedRead,
                    tick,
                    Some(sensor_index),
                );
                continue;
            };
            self.metrics.selected_reads[sensor_index] += 1;
            *selected_tick = Some(selected.edge_tick);
            if let Some(frame) = frame {
                self.metrics.linked_reads[sensor_index] += 1;
                if selected.motion != sensor_motion_bytes(frame.sensors[sensor_index]) {
                    self.violate(
                        Gate0TraceViolationCode::StaleOrWrongPayload,
                        tick,
                        Some(sensor_index),
                    );
                }
            } else {
                self.metrics.host_dropped_reads[sensor_index] += 1;
            }
            let age = tick.saturating_sub(selected.edge_tick);
            self.metrics.max_selected_sample_age_ticks =
                self.metrics.max_selected_sample_age_ticks.max(age);
            if age > MAX_SAMPLE_AGE_TICKS {
                self.violate(
                    Gate0TraceViolationCode::SelectedSampleTooOld,
                    tick,
                    Some(sensor_index),
                );
            }
        }

        let minimum = selected_ticks.iter().flatten().copied().min();
        let maximum = selected_ticks.iter().flatten().copied().max();
        if let (Some(minimum), Some(maximum)) = (minimum, maximum) {
            let spread = maximum - minimum;
            self.metrics.max_snapshot_spread_ticks =
                self.metrics.max_snapshot_spread_ticks.max(spread);
            if spread > MAX_SAMPLE_SPREAD_TICKS {
                self.violate(Gate0TraceViolationCode::SnapshotSpreadExceeded, tick, None);
            }
        }
        if let Some(frame) = frame {
            self.check_sample_offsets(frame, &selected_ticks, tick);
        }
    }

    fn check_sample_offsets(
        &mut self,
        frame: &SensorFrame,
        selected_ticks: &[Option<u64>; SENSOR_COUNT],
        tick: u64,
    ) {
        let Some(reference_tick) = selected_ticks[0] else {
            return;
        };
        let reference_offset =
            i128::from(frame.sensors[0].sample_time_offset_us) * TICKS_PER_MICROSECOND as i128;
        for (sensor_index, selected_tick) in selected_ticks.iter().enumerate().skip(1) {
            let Some(sensor_tick) = selected_tick else {
                continue;
            };
            let analyzer_delta = i128::from(*sensor_tick) - i128::from(reference_tick);
            let session_delta = i128::from(frame.sensors[sensor_index].sample_time_offset_us)
                * TICKS_PER_MICROSECOND as i128
                - reference_offset;
            if (analyzer_delta - session_delta).abs() > SAMPLE_OFFSET_TOLERANCE_TICKS {
                self.violate(
                    Gate0TraceViolationCode::SessionSampleTimeMismatch,
                    tick,
                    Some(sensor_index),
                );
            }
        }
    }

    fn check_normal_marker_width(&mut self, rise_tick: u64, fall_tick: u64) {
        let width = fall_tick.saturating_sub(rise_tick);
        if !(NORMAL_MARKER_MIN_TICKS..BOOT_MARKER_MIN_TICKS).contains(&width) {
            self.violate(Gate0TraceViolationCode::MarkerWidth, fall_tick, None);
        }
    }

    fn finish(&mut self, tick: u64, remaining_session_frames: u64, i2c_active: bool) {
        if self.metrics.startup_end_sample.is_none() {
            self.violate(Gate0TraceViolationCode::StartupIncomplete, tick, None);
        }
        if self.metrics.first_clean_cache_sample.is_none() {
            self.violate(
                Gate0TraceViolationCode::StartupPrimingIncomplete,
                tick,
                None,
            );
        }
        if self.d8_rise_tick.is_some() {
            self.violate(Gate0TraceViolationCode::D8MarkerOrder, tick, None);
        }
        if self.d9_rise_tick.is_some() || matches!(self.read_state, ReadState::AwaitingD9(_)) {
            self.violate(Gate0TraceViolationCode::D9Missing, tick, None);
        }
        if matches!(
            self.read_state,
            ReadState::MuxReadback(_) | ReadState::Status(_) | ReadState::Motion(_)
        ) || i2c_active
        {
            self.violate(Gate0TraceViolationCode::IncompleteReadAtEnd, tick, None);
        }
        for sensor_index in 0..SENSOR_COUNT {
            if self.pending_edges[sensor_index].is_some()
                && self.metrics.final_d8_rise_sample.is_none()
            {
                self.violate(
                    Gate0TraceViolationCode::PendingEdgeAtEnd,
                    tick,
                    Some(sensor_index),
                );
            }
            if self.cache[sensor_index].is_some() {
                self.violate(
                    Gate0TraceViolationCode::UnselectedCacheAtEnd,
                    tick,
                    Some(sensor_index),
                );
            }
        }
        self.violate_repeated(
            Gate0TraceViolationCode::MissingAnalyzerSnapshot,
            tick,
            remaining_session_frames,
        );
        if self.metrics.max_pending_reads != SENSOR_COUNT as u8 {
            self.violate(Gate0TraceViolationCode::MissingSixWayCollision, tick, None);
        }

        if self.metrics.final_d8_rise_sample.is_none()
            || self.metrics.final_d8_fall_sample.is_none()
        {
            self.violate(Gate0TraceViolationCode::FinalSnapshotMismatch, tick, None);
        }
        if self.next_shutdown_transaction != SHUTDOWN_TRANSACTION_COUNT
            || self.metrics.shutdown_transactions != SHUTDOWN_TRANSACTION_COUNT as u64
        {
            self.violate(
                Gate0TraceViolationCode::ShutdownSuffixIncomplete,
                tick,
                None,
            );
        }
        if self.metrics.shutdown_scl_rising_edges != SHUTDOWN_SCL_RISING_EDGES {
            self.violate(Gate0TraceViolationCode::ShutdownSclCount, tick, None);
        }
        let Some(marker_fall) = self.metrics.terminal_marker_fall_sample else {
            self.violate(Gate0TraceViolationCode::TerminalMarkerMissing, tick, None);
            self.metrics.terminal_idle_samples = 0;
            return self.finish_acquisition_metrics(tick);
        };
        self.metrics.terminal_idle_samples = tick.saturating_sub(marker_fall).saturating_add(1);
        if self.metrics.terminal_idle_samples < TERMINAL_IDLE_MINIMUM_SAMPLES {
            self.violate(Gate0TraceViolationCode::TerminalIdleTooShort, tick, None);
        }

        self.finish_acquisition_metrics(tick);
    }

    fn finish_acquisition_metrics(&mut self, tick: u64) {
        let completed_reads: u64 = self.metrics.completed_reads.iter().sum();
        self.metrics.average_status_polls_milli = (completed_reads > 0)
            .then(|| self.metrics.total_status_polls.saturating_mul(1_000) / completed_reads);
        if completed_reads == 0
            || self.metrics.total_status_polls.saturating_mul(1_000)
                >= completed_reads.saturating_mul(4_000)
        {
            self.violate(Gate0TraceViolationCode::PollAverageAtLeastFour, tick, None);
        }

        for sensor_index in 0..SENSOR_COUNT {
            let stats = self.edge_stats[sensor_index];
            if let (Some(first_tick), Some(last_tick)) = (stats.first_tick, stats.last_tick) {
                if self.metrics.int1_edges[sensor_index] >= 2 && last_tick > first_tick {
                    let intervals = self.metrics.int1_edges[sensor_index] - 1;
                    let odr_millihz = intervals
                        .saturating_mul(GATE0_SAMPLE_RATE_HZ)
                        .saturating_mul(1_000)
                        / (last_tick - first_tick);
                    self.metrics.odr_millihz[sensor_index] = Some(odr_millihz);
                    self.metrics.interval_jitter_rms_ticks[sensor_index] = stats.intervals.rms();
                    if !(GATE0_PROVISIONAL_ODR_MIN_MILLIHZ..=GATE0_PROVISIONAL_ODR_MAX_MILLIHZ)
                        .contains(&odr_millihz)
                    {
                        self.violate(
                            Gate0TraceViolationCode::OdrOutOfProvisionalBand,
                            tick,
                            Some(sensor_index),
                        );
                    }
                } else {
                    self.violate(
                        Gate0TraceViolationCode::OdrOutOfProvisionalBand,
                        tick,
                        Some(sensor_index),
                    );
                }
            } else {
                self.violate(
                    Gate0TraceViolationCode::OdrOutOfProvisionalBand,
                    tick,
                    Some(sensor_index),
                );
            }
            if self.metrics.int1_edges[sensor_index] != self.metrics.committed_reads[sensor_index]
                || self.metrics.committed_reads[sensor_index]
                    != self.metrics.selected_reads[sensor_index]
                        + self.metrics.decimated_reads[sensor_index]
                || self.metrics.selected_reads[sensor_index]
                    != self.metrics.linked_reads[sensor_index]
                        + self.metrics.host_dropped_reads[sensor_index]
            {
                self.violate(
                    Gate0TraceViolationCode::AccountingMismatch,
                    tick,
                    Some(sensor_index),
                );
            }
        }

        let histogram_total: u64 = self.metrics.phase_histogram.iter().flatten().copied().sum();
        let decimation_total: u64 = self.metrics.decimated_reads.iter().sum();
        let pre_snapshot_total: u64 = self.metrics.pre_snapshot_decimated_reads.iter().sum();
        if histogram_total + pre_snapshot_total != decimation_total {
            self.violate(Gate0TraceViolationCode::AccountingMismatch, tick, None);
        }
    }

    fn violate_repeated(&mut self, code: Gate0TraceViolationCode, tick: u64, count: u64) {
        let available = MAX_GATE0_TRACE_VIOLATION_WITNESSES
            .saturating_sub(self.witnesses.violations.len()) as u64;
        let retained = count.min(available);
        for _ in 0..retained {
            self.witnesses.violations.push(Gate0TraceViolation {
                code,
                sample_ordinal: tick,
                sensor_index: None,
            });
        }
        self.witnesses.violation_count = self.witnesses.violation_count.saturating_add(count);
        self.witnesses.violation_overflow = self
            .witnesses
            .violation_overflow
            .saturating_add(count - retained);
    }
}

fn is_status_transfer(transaction: &I2cTransaction, write: &[u8], read: &[u8]) -> bool {
    transaction.address == IMU_ADDRESS
        && write == [STATUS_REGISTER]
        && read.len() == 1
        && transaction.scl_pulses == 36
}

fn shutdown_transaction_matches(index: usize, transaction: &I2cTransaction) -> bool {
    if index == 0 || index == SHUTDOWN_TRANSACTION_COUNT - 2 {
        return transfer_matches(transaction, MUX_ADDRESS, &[0], &[], 18);
    }
    if index == 1 || index == SHUTDOWN_TRANSACTION_COUNT - 1 {
        return transfer_matches(transaction, MUX_ADDRESS, &[], &[0], 18);
    }

    let sensor_step = index - 2;
    let sensor_index = sensor_step / 8;
    let step = sensor_step % 8;
    if sensor_index >= SENSOR_COUNT {
        return false;
    }
    let mux_mask = 1_u8 << sensor_index;
    match step {
        0 => transfer_matches(transaction, MUX_ADDRESS, &[mux_mask], &[], 18),
        1 => transfer_matches(transaction, MUX_ADDRESS, &[], &[mux_mask], 18),
        step => {
            let register = SHUTDOWN_REGISTERS[(step - 2) / 2];
            if step % 2 == 0 {
                transfer_matches(transaction, IMU_ADDRESS, &[register, 0], &[], 27)
            } else {
                transfer_matches(transaction, IMU_ADDRESS, &[register], &[0], 36)
            }
        }
    }
}

fn transfer_matches(
    transaction: &I2cTransaction,
    address: u8,
    write: &[u8],
    read: &[u8],
    scl_pulses: u16,
) -> bool {
    transaction.address == address
        && &transaction.write[..transaction.write_len] == write
        && &transaction.read[..transaction.read_len] == read
        && transaction.scl_pulses == scl_pulses
}

fn status_ready(status: u8) -> bool {
    status & STATUS_READY_MASK == STATUS_READY_MASK
}

fn odr_in_provisional_band(interval_ticks: u64) -> bool {
    (MIN_PROVISIONAL_ODR_INTERVAL_TICKS..=MAX_PROVISIONAL_ODR_INTERVAL_TICKS)
        .contains(&interval_ticks)
}

fn sensor_motion_bytes(reading: SensorReading) -> [u8; 12] {
    let mut bytes = [0_u8; 12];
    for (index, value) in reading
        .angular_velocity
        .into_iter()
        .chain(reading.acceleration)
        .enumerate()
    {
        bytes[index * 2..index * 2 + 2].copy_from_slice(&value.to_le_bytes());
    }
    bytes
}

#[derive(Debug)]
struct ValidatedSidecar {
    raw_capture_sha256: [u8; 32],
    raw_capture_bytes: u64,
    sidecar_sha256: [u8; 32],
    settings: Gate0CaptureSettings,
    capture_plan_matches: bool,
}

/// State carried between changed words in the continuous raw stream.
///
/// The first word is peeled before this state exists. That keeps the hot path
/// free of an `Option` check for each of the 7.22 billion canonical samples.
struct RawStreamState {
    previous_word: u16,
    boot: BootAnchorState,
    i2c: I2cDecoder,
    reserved_bits_active: bool,
    preboot_bus_idle_seen: bool,
}

impl RawStreamState {
    fn new(first_word: u16, reducer: &mut TraceReducer) -> Self {
        reducer.metrics.raw_samples = 1;
        let reserved_bits_active = first_word & !USED_LOGIC_MASK != 0;
        if reserved_bits_active {
            reducer.violate(Gate0TraceViolationCode::ReservedLogicBitSet, 0, None);
        }
        let mut boot = BootAnchorState::default();
        if first_word & (INT1_MASK | D8_MASK | D9_MASK) != 0 {
            reducer.violate(Gate0TraceViolationCode::BootAnchorOrder, 0, None);
            boot = BootAnchorState::Failed;
        }
        Self {
            previous_word: first_word,
            boot,
            i2c: I2cDecoder::default(),
            reserved_bits_active,
            preboot_bus_idle_seen: first_word & (SDA_MASK | SCL_MASK) == SDA_MASK | SCL_MASK,
        }
    }

    fn process_word(
        &mut self,
        word: u16,
        reducer: &mut TraceReducer,
        session_cursor: &mut SessionCursor<'_>,
    ) -> Result<(), Gate0TraceError> {
        let tick = reducer.metrics.raw_samples;
        reducer.metrics.raw_samples += 1;
        let previous = self.previous_word;
        let raw_delta = previous ^ word;
        if raw_delta == 0 {
            if matches!(self.boot, BootAnchorState::LowGuard { .. }) {
                advance_boot_guard(&mut self.boot, word, tick, reducer);
            }
            return Ok(());
        }
        self.previous_word = word;

        let reserved_now = word & !USED_LOGIC_MASK != 0;
        if reserved_now && !self.reserved_bits_active {
            reducer.violate(Gate0TraceViolationCode::ReservedLogicBitSet, tick, None);
        }
        self.reserved_bits_active = reserved_now;

        let changed = raw_delta & USED_LOGIC_MASK;
        if changed == 0 {
            advance_boot_guard(&mut self.boot, word, tick, reducer);
            if matches!(self.boot, BootAnchorState::Complete) {
                match reducer.quiesce_phase {
                    QuiescePhase::MarkerHigh => {
                        reducer.violate(Gate0TraceViolationCode::TerminalMarkerShape, tick, None)
                    }
                    QuiescePhase::Quiescent => {
                        reducer.violate(Gate0TraceViolationCode::PostMarkerActivity, tick, None)
                    }
                    _ => {}
                }
                reducer.observe_premarker_word(word, tick);
            }
            return Ok(());
        }
        reducer.metrics.raw_transitions += 1;

        advance_boot_guard(&mut self.boot, word, tick, reducer);
        if !matches!(self.boot, BootAnchorState::Complete) {
            observe_boot_transition(
                &mut self.boot,
                &mut self.preboot_bus_idle_seen,
                previous,
                word,
                changed,
                tick,
                reducer,
            );
            return Ok(());
        }

        let final_d8_rise = reducer.is_final_d8_rise(previous, word);
        if final_d8_rise {
            let sequence = reducer.metrics.snapshots as u32;
            let (frame, missing_snapshots) = session_cursor.frame_for_sequence(sequence)?;
            reducer.violate_repeated(
                Gate0TraceViolationCode::MissingAnalyzerSnapshot,
                tick,
                missing_snapshots,
            );
            reducer.d8_edge(tick, true, frame);
            if self.i2c.active {
                reducer.final_cut_has_active_i2c(tick);
            }
        }
        if !self.i2c.active
            && changed & (SDA_MASK | SCL_MASK) == SDA_MASK
            && word & (SDA_MASK | SCL_MASK) == SCL_MASK
        {
            reducer.startup_i2c_start();
        }

        let mut rising_int1 = !previous & word & INT1_MASK;
        while rising_int1 != 0 {
            let sensor_index = rising_int1.trailing_zeros() as usize;
            match reducer.quiesce_phase {
                QuiescePhase::Initializing
                    if reducer.startup.admitted_sensor_mask & (1_u8 << sensor_index) != 0 =>
                {
                    reducer.int1_rise(tick, sensor_index);
                }
                QuiescePhase::Initializing => {
                    reducer.metrics.startup_unadmitted_int1_edges[sensor_index] += 1;
                    if reducer.startup.quiet_sensor_mask & (1_u8 << sensor_index) != 0 {
                        reducer.violate(
                            Gate0TraceViolationCode::StartupKnownQuietInt1,
                            tick,
                            Some(sensor_index),
                        );
                    }
                }
                QuiescePhase::Acquiring => reducer.int1_rise(tick, sensor_index),
                _ => reducer.shutdown_int1_rise(sensor_index),
            }
            rising_int1 &= rising_int1 - 1;
        }

        if reducer.terminal_marker_transition(previous, word, changed, tick) {
            return Ok(());
        }

        if changed & (SDA_MASK | SCL_MASK) != 0 && changed & (D8_MASK | D9_MASK) != 0 {
            reducer.violate(
                Gate0TraceViolationCode::MarkerBusTransitionOverlap,
                tick,
                None,
            );
        }
        if changed & (SDA_MASK | SCL_MASK) != 0 {
            match self.i2c.observe(previous, word, tick, &mut reducer.metrics) {
                Ok(Some(transaction)) => reducer.transaction(transaction),
                Ok(None) => {}
                Err(I2cFault::AmbiguousTransition) => {
                    reducer.classify_i2c_fault(tick);
                    reducer.violate(Gate0TraceViolationCode::AmbiguousBusTransition, tick, None);
                    self.i2c = I2cDecoder::default();
                }
                Err(I2cFault::Protocol | I2cFault::TooManyBytes) => {
                    reducer.classify_i2c_fault(tick);
                    reducer.violate(Gate0TraceViolationCode::I2cProtocol, tick, None);
                    self.i2c = I2cDecoder::default();
                }
            }
        }

        if changed & (D8_MASK | D9_MASK) == (D8_MASK | D9_MASK) {
            reducer.violate(Gate0TraceViolationCode::D8D9Overlap, tick, None);
        }
        if changed & D9_MASK != 0 {
            reducer.d9_edge(tick, word & D9_MASK != 0);
        }
        if changed & D8_MASK != 0 {
            if final_d8_rise {
                return Ok(());
            }
            let asserted = word & D8_MASK != 0;
            let frame = if asserted && reducer.quiesce_phase != QuiescePhase::Initializing {
                let sequence = reducer.metrics.snapshots as u32;
                let (frame, missing_snapshots) = session_cursor.frame_for_sequence(sequence)?;
                reducer.violate_repeated(
                    Gate0TraceViolationCode::MissingAnalyzerSnapshot,
                    tick,
                    missing_snapshots,
                );
                frame
            } else {
                None
            };
            reducer.d8_edge(tick, asserted, frame);
        }
        reducer.observe_premarker_word(word, tick);
        Ok(())
    }
}

/// Open and independently reduce the native DSLogic Gate 0A capture.
///
/// The reducer opens and hashes the sidecar, raw u16 sample stream, capture
/// binary, and full capture log. It derives digital transitions and I2C
/// transactions directly from every raw sample. The implementation retains
/// only fixed per-sensor/cache state, one I2C transaction, running totals, and
/// a bounded witness list. A 14.44 GB canonical capture therefore does not
/// require memory proportional to capture length.
///
/// The sidecar is provenance, not scored evidence. Its raw hash and size must
/// match the bytes this function reads. Its capture log must contain exactly
/// one patched-driver success record and no known transfer anomaly. Semantic
/// waveform failures return evidence with `is_clean() == false`; malformed or
/// mismatched source artifacts return an error and cannot be scored.
///
/// # Errors
///
/// Returns an error for malformed or oversized sidecars, unsafe artifact
/// paths, hash or size mismatches, rejected capture logs, invalid raw word
/// framing, manifest errors, or an internal finalized-session replay error.
pub fn reduce_gate0_capture(
    manifest: &AcquisitionManifest,
    session: &FinalizedSession,
    paths: Gate0CapturePaths<'_>,
) -> Result<Gate0TraceEvidence, Gate0TraceError> {
    let manifest_sha256 = manifest.sha256()?;
    let target_identity = one_hand_identity(manifest)?;
    let terminal = session.gate0a_terminal_binding()?;
    if terminal.source != ControllerStream::from(target_identity.hand)
        || terminal.final_frame.identity() != target_identity
    {
        return Err(Gate0TraceError::TerminalBindingMismatch);
    }
    let sidecar = validate_sidecar(
        paths.sidecar,
        paths.raw_capture,
        session.bytes().len(),
        &manifest_sha256,
    )?;

    let mut reducer = TraceReducer::new(
        target_identity,
        terminal.receipt.final_sequence,
        terminal.response_time_ns,
    );
    if !sidecar.capture_plan_matches {
        reducer.violate(Gate0TraceViolationCode::CapturePlanMismatch, 0, None);
    }
    let mut session_cursor = SessionCursor::new(session);
    let raw_capture_sha256 = reduce_raw_file(
        paths.raw_capture,
        sidecar.raw_capture_bytes,
        &mut reducer,
        &mut session_cursor,
    )?;
    if raw_capture_sha256 != sidecar.raw_capture_sha256 {
        return Err(Gate0TraceError::RawCaptureHashMismatch);
    }
    if reducer.metrics.raw_samples < GATE0_MINIMUM_SAMPLES {
        let last_tick = reducer.metrics.raw_samples.saturating_sub(1);
        reducer.violate(Gate0TraceViolationCode::CaptureTooShort, last_tick, None);
    }

    Ok(Gate0TraceEvidence {
        schema: GATE0_CAPTURE_SIDECAR_SCHEMA,
        manifest_sha256,
        session_sha256: session.sha256(),
        raw_capture_sha256,
        sidecar_sha256: sidecar.sidecar_sha256,
        settings: sidecar.settings,
        metrics: reducer.metrics,
        witnesses: reducer.witnesses,
    })
}

fn reduce_raw_file(
    path: &Path,
    expected_bytes: u64,
    reducer: &mut TraceReducer,
    session_cursor: &mut SessionCursor<'_>,
) -> Result<[u8; 32], Gate0TraceError> {
    let mut file = File::open(path)?;
    let metadata = file.metadata()?;
    if metadata.len() == 0 {
        return Err(Gate0TraceError::EmptyRawCapture);
    }
    if metadata.len() != expected_bytes {
        return Err(Gate0TraceError::ArtifactSizeMismatch("raw capture"));
    }
    if metadata.len() % u64::from(RAW_WORD_BYTES) != 0 {
        return Err(Gate0TraceError::OddRawCaptureLength);
    }

    let mut hasher = Sha256::new();
    let mut first_word_bytes = [0_u8; 2];
    file.read_exact(&mut first_word_bytes).map_err(|error| {
        if error.kind() == io::ErrorKind::UnexpectedEof {
            Gate0TraceError::OddRawCaptureLength
        } else {
            Gate0TraceError::Io(error)
        }
    })?;
    hasher.update(first_word_bytes);
    let mut bytes_read = 2_u64;
    let mut raw_state = RawStreamState::new(u16::from_le_bytes(first_word_bytes), reducer);
    let mut buffer = [0_u8; 128 * 1024];
    let mut pending_byte = None;

    loop {
        let bytes = file.read(&mut buffer)?;
        if bytes == 0 {
            break;
        }
        hasher.update(&buffer[..bytes]);
        bytes_read = bytes_read.saturating_add(bytes as u64);
        if bytes_read > expected_bytes {
            return Err(Gate0TraceError::ArtifactSizeMismatch("raw capture"));
        }
        let mut offset = 0;
        if let Some(low) = pending_byte.take() {
            let word = u16::from_le_bytes([low, buffer[0]]);
            raw_state.process_word(word, reducer, session_cursor)?;
            offset = 1;
        }
        while offset + 1 < bytes {
            let word = u16::from_le_bytes([buffer[offset], buffer[offset + 1]]);
            raw_state.process_word(word, reducer, session_cursor)?;
            offset += 2;
        }
        if offset < bytes {
            pending_byte = Some(buffer[offset]);
        }
    }
    if pending_byte.is_some() {
        return Err(Gate0TraceError::OddRawCaptureLength);
    }
    if bytes_read != expected_bytes {
        return Err(Gate0TraceError::ArtifactSizeMismatch("raw capture"));
    }

    let final_tick = reducer.metrics.raw_samples.saturating_sub(1);
    if !matches!(raw_state.boot, BootAnchorState::Complete) {
        reducer.violate(Gate0TraceViolationCode::BootAnchorMissing, final_tick, None);
    }
    let remaining_session_frames = session_cursor.remaining_frames()?;
    reducer.finish(final_tick, remaining_session_frames, raw_state.i2c.active);
    Ok(hasher.finalize().into())
}

fn advance_boot_guard(
    boot: &mut BootAnchorState,
    word: u16,
    tick: u64,
    reducer: &mut TraceReducer,
) {
    let BootAnchorState::LowGuard { fall_tick } = *boot else {
        return;
    };
    if word & (D8_MASK | D9_MASK) != 0 || word & (SDA_MASK | SCL_MASK) != (SDA_MASK | SCL_MASK) {
        return;
    }
    if tick.saturating_sub(fall_tick) >= BOOT_MARKER_MIN_TICKS {
        *boot = BootAnchorState::Complete;
        reducer.witnesses.boot_anchor_end = Some(Gate0TracePointWitness {
            sample_ordinal: tick,
            sensor_index: None,
            value: tick,
        });
    }
}

fn observe_boot_transition(
    boot: &mut BootAnchorState,
    preboot_bus_idle_seen: &mut bool,
    previous: u16,
    word: u16,
    changed: u16,
    tick: u64,
    reducer: &mut TraceReducer,
) {
    // Once the signature is known to be invalid, retain the first cause and let
    // finish() add one BootAnchorMissing witness.  Continuing to classify every
    // later edge would fill the bounded witness list with the same root cause.
    if matches!(boot, BootAnchorState::Failed) {
        return;
    }
    if changed & INT1_MASK != 0 {
        reducer.violate(
            Gate0TraceViolationCode::ActivityBeforeBootAnchor,
            tick,
            None,
        );
        *boot = BootAnchorState::Failed;
        return;
    }
    if changed & (SDA_MASK | SCL_MASK) != 0 {
        if matches!(boot, BootAnchorState::AwaitD8Rise) {
            // The analyzer is armed before power-on, so the two pull-ups may
            // rise separately. Once both lines have reached idle-high, any
            // further bus edge before D8 is real pre-anchor bus activity.
            if !*preboot_bus_idle_seen {
                *preboot_bus_idle_seen = word & (SDA_MASK | SCL_MASK) == SDA_MASK | SCL_MASK;
                return;
            }
        }
        reducer.violate(
            Gate0TraceViolationCode::ActivityBeforeBootAnchor,
            tick,
            None,
        );
        *boot = BootAnchorState::Failed;
        return;
    }
    let d8_rising = previous & D8_MASK == 0 && word & D8_MASK != 0;
    let d8_falling = previous & D8_MASK != 0 && word & D8_MASK == 0;
    let d9_rising = previous & D9_MASK == 0 && word & D9_MASK != 0;
    let d9_falling = previous & D9_MASK != 0 && word & D9_MASK == 0;
    match *boot {
        BootAnchorState::AwaitD8Rise if d8_rising && changed == D8_MASK => {
            *boot = BootAnchorState::D8High { rise_tick: tick };
        }
        BootAnchorState::D8High { rise_tick } if d8_falling && changed == D8_MASK => {
            if !boot_width_valid(tick.saturating_sub(rise_tick)) {
                reducer.violate(Gate0TraceViolationCode::BootAnchorWidth, tick, None);
                *boot = BootAnchorState::Failed;
            } else {
                *boot = BootAnchorState::D8LowGap { fall_tick: tick };
            }
        }
        BootAnchorState::D8LowGap { fall_tick } if d9_rising && changed == D9_MASK => {
            if !boot_width_valid(tick.saturating_sub(fall_tick)) {
                reducer.violate(Gate0TraceViolationCode::BootAnchorWidth, tick, None);
                *boot = BootAnchorState::Failed;
            } else {
                *boot = BootAnchorState::D9High { rise_tick: tick };
            }
        }
        BootAnchorState::D9High { rise_tick } if d9_falling && changed == D9_MASK => {
            if !boot_width_valid(tick.saturating_sub(rise_tick)) {
                reducer.violate(Gate0TraceViolationCode::BootAnchorWidth, tick, None);
                *boot = BootAnchorState::Failed;
            } else {
                *boot = BootAnchorState::LowGuard { fall_tick: tick };
            }
        }
        BootAnchorState::Failed => {}
        _ => {
            reducer.violate(Gate0TraceViolationCode::BootAnchorOrder, tick, None);
            *boot = BootAnchorState::Failed;
        }
    }
}

fn boot_width_valid(ticks: u64) -> bool {
    (BOOT_MARKER_MIN_TICKS..=BOOT_MARKER_MAX_TICKS).contains(&ticks)
}

fn one_hand_identity(manifest: &AcquisitionManifest) -> Result<FrameIdentity, Gate0TraceError> {
    let identities = manifest.frame_identities()?;
    if identities.len() != 1 {
        return Err(Gate0TraceError::InvalidSidecarField(
            "manifest must declare exactly one hand",
        ));
    }
    Ok(identities[0])
}

fn validate_sidecar(
    path: &Path,
    raw_capture_path: &Path,
    session_bytes: usize,
    locked_manifest_sha256: &[u8; 32],
) -> Result<ValidatedSidecar, Gate0TraceError> {
    let bytes = read_bounded_file(path, MAX_GATE0_CAPTURE_SIDECAR_BYTES)?;
    let sidecar_sha256 = sha256(&bytes);
    let sidecar: CaptureSidecar = serde_json::from_slice(&bytes)
        .map_err(|error| Gate0TraceError::InvalidSidecar(error.to_string()))?;
    if sidecar.schema != GATE0_CAPTURE_SIDECAR_SCHEMA {
        return Err(Gate0TraceError::UnsupportedSidecarSchema(sidecar.schema));
    }
    if sidecar.word_bytes != RAW_WORD_BYTES {
        return Err(Gate0TraceError::InvalidSidecarField("word_bytes"));
    }
    if sidecar.byte_order != RAW_BYTE_ORDER {
        return Err(Gate0TraceError::InvalidSidecarField("byte_order"));
    }
    if sidecar.sample_rate_hz != GATE0_SAMPLE_RATE_HZ {
        return Err(Gate0TraceError::InvalidSidecarField("sample_rate_hz"));
    }
    if sidecar.channel_map != Gate0ChannelMap::canonical() {
        return Err(Gate0TraceError::InvalidSidecarField("channel_map"));
    }
    if sidecar.voltage_threshold_low_mv != GATE0_VOLTAGE_THRESHOLD_LOW_MV {
        return Err(Gate0TraceError::InvalidSidecarField(
            "voltage_threshold_low_mv",
        ));
    }
    if sidecar.voltage_threshold_high_mv != GATE0_VOLTAGE_THRESHOLD_HIGH_MV {
        return Err(Gate0TraceError::InvalidSidecarField(
            "voltage_threshold_high_mv",
        ));
    }
    if sidecar.capture_finished_unix_ms <= sidecar.capture_started_unix_ms {
        return Err(Gate0TraceError::InvalidSidecarField(
            "capture_finished_unix_ms",
        ));
    }
    if sidecar.capture_exit_status != 0 {
        return Err(Gate0TraceError::InvalidSidecarField("capture_exit_status"));
    }
    if sidecar.driver_base_commit != PATCHED_DRIVER_BASE_COMMIT {
        return Err(Gate0TraceError::InvalidSidecarField("driver_base_commit"));
    }
    if sidecar.sigrok_cli_base_commit != PATCHED_SIGROK_CLI_BASE_COMMIT {
        return Err(Gate0TraceError::InvalidSidecarField(
            "sigrok_cli_base_commit",
        ));
    }
    if sidecar.device_model != GATE0_DEVICE_MODEL {
        return Err(Gate0TraceError::InvalidSidecarField("device_model"));
    }

    let raw_capture_sha256 =
        parse_required_hash(&sidecar.raw_capture_sha256, "raw_capture_sha256", false)?;
    let driver_patch_sha256 =
        parse_required_hash(&sidecar.driver_patch_sha256, "driver_patch_sha256", true)?;
    let base = path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .canonicalize()?;
    let capture_binary_sha256 =
        validate_bound_artifact(&base, &sidecar.capture_binary, "capture binary")?;
    let device_firmware_bundle_sha256 = validate_bound_artifact(
        &base,
        &sidecar.device_firmware_bundle,
        "device firmware bundle",
    )?;
    let capture_log_sha256 = validate_capture_log_artifact(
        &base,
        &sidecar.capture_log,
        sidecar.requested_samples,
        session_bytes,
        locked_manifest_sha256,
        &device_firmware_bundle_sha256,
    )?;
    validate_command(
        &sidecar.command,
        sidecar.requested_samples,
        &sidecar.capture_binary.path,
        raw_capture_path,
    )?;

    let expected_raw_bytes = sidecar.requested_samples.checked_mul(2);
    let capture_plan_matches = sidecar.requested_samples == GATE0_REQUESTED_SAMPLES
        && expected_raw_bytes == Some(sidecar.raw_capture_bytes);
    Ok(ValidatedSidecar {
        raw_capture_sha256,
        raw_capture_bytes: sidecar.raw_capture_bytes,
        sidecar_sha256,
        settings: Gate0CaptureSettings {
            sample_rate_hz: sidecar.sample_rate_hz,
            word_bytes: sidecar.word_bytes,
            byte_order: sidecar.byte_order,
            channel_map: sidecar.channel_map,
            voltage_threshold_low_mv: sidecar.voltage_threshold_low_mv,
            voltage_threshold_high_mv: sidecar.voltage_threshold_high_mv,
            requested_samples: sidecar.requested_samples,
            capture_started_unix_ms: sidecar.capture_started_unix_ms,
            capture_finished_unix_ms: sidecar.capture_finished_unix_ms,
            capture_binary_sha256,
            capture_log_sha256,
            device_firmware_bundle_sha256,
            driver_base_commit: sidecar.driver_base_commit,
            sigrok_cli_base_commit: sidecar.sigrok_cli_base_commit,
            driver_patch_sha256,
            device_model: sidecar.device_model,
        },
        capture_plan_matches,
    })
}

fn read_bounded_file(path: &Path, limit: u64) -> Result<Vec<u8>, Gate0TraceError> {
    let mut file = File::open(path)?;
    let metadata = file.metadata()?;
    if metadata.len() > limit {
        return Err(Gate0TraceError::SidecarTooLarge);
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    (&mut file).take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(Gate0TraceError::SidecarTooLarge);
    }
    Ok(bytes)
}

fn parse_required_hash(
    value: &str,
    field: &'static str,
    reject_zero: bool,
) -> Result<[u8; 32], Gate0TraceError> {
    let hash = parse_sha256(value).ok_or(Gate0TraceError::InvalidSidecarField(field))?;
    if reject_zero && hash == [0; 32] {
        return Err(Gate0TraceError::InvalidSidecarField(field));
    }
    Ok(hash)
}

fn validate_bound_artifact(
    canonical_base: &Path,
    artifact: &BoundArtifact,
    label: &'static str,
) -> Result<[u8; 32], Gate0TraceError> {
    if artifact.bytes == 0 {
        return Err(Gate0TraceError::ArtifactSizeMismatch(label));
    }
    let path = resolve_artifact_path(canonical_base, &artifact.path, label)?;
    let mut file = File::open(path)?;
    let metadata = file.metadata()?;
    if metadata.len() != artifact.bytes {
        return Err(Gate0TraceError::ArtifactSizeMismatch(label));
    }
    let expected = parse_required_hash(&artifact.sha256, label, true)?;
    let (actual, actual_bytes) = hash_reader(&mut file, artifact.bytes)?;
    if actual_bytes != artifact.bytes {
        return Err(Gate0TraceError::ArtifactSizeMismatch(label));
    }
    if actual != expected {
        return Err(Gate0TraceError::ArtifactHashMismatch(label));
    }
    Ok(actual)
}

fn resolve_artifact_path(
    canonical_base: &Path,
    value: &str,
    field: &'static str,
) -> Result<PathBuf, Gate0TraceError> {
    if value.is_empty() || value.len() > MAX_GATE0_CAPTURE_STRING_BYTES {
        return Err(Gate0TraceError::InvalidArtifactPath(field));
    }
    let relative = Path::new(value);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(Gate0TraceError::InvalidArtifactPath(field));
    }
    let canonical_path = canonical_base.join(relative).canonicalize()?;
    if !canonical_path.starts_with(canonical_base) {
        return Err(Gate0TraceError::InvalidArtifactPath(field));
    }
    Ok(canonical_path)
}

fn hash_reader(
    reader: &mut impl Read,
    expected_bytes: u64,
) -> Result<([u8; 32], u64), Gate0TraceError> {
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut total = 0_u64;
    loop {
        let bytes = reader.read(&mut buffer)?;
        if bytes == 0 {
            break;
        }
        total = total.saturating_add(bytes as u64);
        if total > expected_bytes {
            break;
        }
        hasher.update(&buffer[..bytes]);
    }
    Ok((hasher.finalize().into(), total))
}

fn validate_capture_log_artifact(
    canonical_base: &Path,
    artifact: &BoundArtifact,
    requested_samples: u64,
    session_bytes: usize,
    locked_manifest_sha256: &[u8; 32],
    firmware_bundle_sha256: &[u8; 32],
) -> Result<[u8; 32], Gate0TraceError> {
    if artifact.bytes == 0 {
        return Err(Gate0TraceError::ArtifactSizeMismatch("capture log"));
    }
    let path = resolve_artifact_path(canonical_base, &artifact.path, "capture log")?;
    let expected_hash = parse_required_hash(&artifact.sha256, "capture log", true)?;
    let file = File::open(path)?;
    if file.metadata()?.len() != artifact.bytes {
        return Err(Gate0TraceError::ArtifactSizeMismatch("capture log"));
    }

    // Hash and inspect the same open file description. Semantic errors are
    // retained until EOF so a hash or final-size mismatch wins over claims
    // parsed from bytes that are not the sidecar-bound log.
    let mut reader = BufReader::new(file);
    let mut hasher = Sha256::new();
    let mut total_bytes = 0_u64;
    let mut line = Vec::with_capacity(256);
    let mut discarding_oversized_line = false;
    let mut semantic_error = None;
    let mut summary_count = 0_u8;
    let mut summary_transfers = None;
    let mut driver_receipt_count = 0_u8;
    let mut driver_transfers = None;
    let mut firmware_receipt_counts = [0_u8; FIRMWARE_RECEIPTS.len()];
    let mut final_line_is_summary = false;
    let expected_locked_manifest_sha256 = format_sha256(locked_manifest_sha256);
    let expected_firmware_bundle_sha256 = format_sha256(firmware_bundle_sha256);
    loop {
        line.clear();
        let bytes = (&mut reader).take(4_097).read_until(b'\n', &mut line)?;
        if bytes == 0 {
            break;
        }
        total_bytes = total_bytes.saturating_add(bytes as u64);
        if total_bytes > artifact.bytes {
            return Err(Gate0TraceError::ArtifactSizeMismatch("capture log"));
        }
        hasher.update(&line);
        let lf_terminated = line.last() == Some(&b'\n');

        if discarding_oversized_line {
            discarding_oversized_line = !lf_terminated;
            continue;
        }
        if bytes > 4_096 {
            semantic_error.get_or_insert_with(|| {
                Gate0TraceError::CaptureLogRejected("one log line exceeds 4096 bytes".to_owned())
            });
            discarding_oversized_line = !lf_terminated;
            final_line_is_summary = false;
            continue;
        }
        if lf_terminated {
            line.pop();
        }
        let Ok(text) = std::str::from_utf8(&line) else {
            semantic_error.get_or_insert_with(|| {
                Gate0TraceError::CaptureLogRejected("log is not UTF-8".to_owned())
            });
            final_line_is_summary = false;
            continue;
        };
        final_line_is_summary = false;
        if text.starts_with(CAPTURE_LOG_SUMMARY_PREFIX) {
            if !lf_terminated {
                semantic_error.get_or_insert_with(|| {
                    Gate0TraceError::CaptureLogRejected(
                        "patched-driver success summary is not LF-terminated".to_owned(),
                    )
                });
            }
            match validate_capture_summary(
                text,
                requested_samples,
                session_bytes,
                &expected_locked_manifest_sha256,
                &expected_firmware_bundle_sha256,
            ) {
                Ok(transfers) => {
                    summary_transfers = Some(transfers);
                    summary_count = summary_count.saturating_add(1);
                    final_line_is_summary = lf_terminated;
                }
                Err(error) => {
                    semantic_error.get_or_insert(error);
                }
            }
        }
        let driver_receipts_on_line = text.matches(DRIVER_RECEIPT_PREFIX).count();
        if driver_receipts_on_line > 0 {
            match parse_driver_receipt(text) {
                Ok(transfers) => driver_transfers = Some(transfers),
                Err(error) => {
                    semantic_error.get_or_insert(error);
                }
            }
            driver_receipt_count = driver_receipt_count
                .saturating_add(u8::try_from(driver_receipts_on_line).unwrap_or(u8::MAX));
        }
        for (index, expected) in FIRMWARE_RECEIPTS.iter().enumerate() {
            if text == *expected {
                firmware_receipt_counts[index] = firmware_receipt_counts[index].saturating_add(1);
            }
        }
        let lower = text.to_ascii_lowercase();
        for rejected in [
            "ikb_dslogic_transfer_failure_v1",
            "capture rejected",
            "critical",
            "warning",
            "invalid transfer length",
            "empty transfer",
            "zero-length transfer",
            "usb transfer failed",
            "transfer cancelled",
            "transfer timed out",
            "transfer stall",
            "transfer overflow",
        ] {
            if lower.contains(rejected) {
                semantic_error.get_or_insert_with(|| {
                    Gate0TraceError::CaptureLogRejected(rejected.to_owned())
                });
            }
        }
    }
    if summary_count != 1 || !final_line_is_summary {
        semantic_error.get_or_insert_with(|| {
            Gate0TraceError::CaptureLogRejected(
                "expected exactly one final patched-driver success summary".to_owned(),
            )
        });
    }
    if driver_receipt_count != 1 || driver_transfers != summary_transfers {
        semantic_error.get_or_insert_with(|| {
            Gate0TraceError::CaptureLogRejected(
                "patched-driver and wrapper transfer receipts do not match".to_owned(),
            )
        });
    }
    if firmware_receipt_counts != [1; FIRMWARE_RECEIPTS.len()] {
        semantic_error.get_or_insert_with(|| {
            Gate0TraceError::CaptureLogRejected(
                "loaded-firmware receipts do not match the fixed DSLogic Plus bundle".to_owned(),
            )
        });
    }
    if total_bytes != artifact.bytes {
        return Err(Gate0TraceError::ArtifactSizeMismatch("capture log"));
    }
    let actual_hash = hasher.finalize().into();
    if actual_hash != expected_hash {
        return Err(Gate0TraceError::ArtifactHashMismatch("capture log"));
    }
    if let Some(error) = semantic_error {
        return Err(error);
    }
    Ok(actual_hash)
}

fn validate_capture_summary(
    text: &str,
    requested_samples: u64,
    session_bytes: usize,
    locked_manifest_sha256: &str,
    firmware_bundle_sha256: &str,
) -> Result<u64, Gate0TraceError> {
    let mut split_fields = text.split(' ');
    let fields: [&str; 8] = std::array::from_fn(|_| split_fields.next().unwrap_or_default());
    if split_fields.next().is_some()
        || fields[0] != "IKB_GATE0_CAPTURE_V2"
        || fields[3] != "anomalies=0"
    {
        return Err(Gate0TraceError::CaptureLogRejected(
            "malformed patched-driver success summary".to_owned(),
        ));
    }
    let samples = fields[1]
        .strip_prefix("samples=")
        .and_then(|value| value.parse::<u64>().ok());
    let transfers = fields[2]
        .strip_prefix("completed_transfers=")
        .and_then(|value| value.parse::<u64>().ok());
    let stop_observed_raw_bytes = fields[4]
        .strip_prefix("stop_observed_raw_bytes=")
        .and_then(|value| value.parse::<u64>().ok());
    let receipt_session_bytes = fields[5]
        .strip_prefix("session_bytes=")
        .and_then(|value| value.parse::<usize>().ok());
    let manifest_hash = fields[6].strip_prefix("locked_manifest_sha256=");
    let firmware_hash = fields[7].strip_prefix("firmware_bundle_sha256=");
    if samples != Some(requested_samples)
        || transfers == Some(0)
        || transfers.is_none()
        || !stop_observed_raw_bytes
            .is_some_and(|bytes| (STOP_WINDOW_FIRST_BYTE..=STOP_WINDOW_LAST_BYTE).contains(&bytes))
        || receipt_session_bytes != Some(session_bytes)
        || manifest_hash != Some(locked_manifest_sha256)
        || firmware_hash != Some(firmware_bundle_sha256)
    {
        return Err(Gate0TraceError::CaptureLogRejected(
            "patched-driver summary does not bind the capture plan and terminal artifacts"
                .to_owned(),
        ));
    }
    Ok(transfers.expect("validated positive transfer count"))
}

fn parse_driver_receipt(text: &str) -> Result<u64, Gate0TraceError> {
    let receipt = text
        .split_once(DRIVER_RECEIPT_PREFIX)
        .map(|(_, receipt)| receipt)
        .ok_or_else(|| {
            Gate0TraceError::CaptureLogRejected("malformed patched-driver receipt".to_owned())
        })?;
    let mut fields = receipt.split_whitespace();
    let transfers = fields
        .next()
        .and_then(|field| field.strip_prefix("completed_transfers="))
        .and_then(|value| value.parse::<u64>().ok());
    if transfers == Some(0)
        || transfers.is_none()
        || fields.next() != Some("anomalies=0")
        || fields.next().is_some()
    {
        return Err(Gate0TraceError::CaptureLogRejected(
            "malformed patched-driver receipt".to_owned(),
        ));
    }
    Ok(transfers.expect("validated positive transfer count"))
}

fn validate_command(
    command: &[String],
    requested_samples: u64,
    capture_binary_path: &str,
    raw_capture_path: &Path,
) -> Result<(), Gate0TraceError> {
    if command.len() != 15 {
        return Err(Gate0TraceError::InvalidSidecarField("command"));
    }
    if command
        .iter()
        .any(|value| value.is_empty() || value.len() > MAX_GATE0_CAPTURE_STRING_BYTES)
    {
        return Err(Gate0TraceError::InvalidSidecarField("command"));
    }
    let published_name = Path::new(capture_binary_path)
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(Gate0TraceError::InvalidSidecarField("command"))?;
    let expected_capture_name = format!(".{published_name}.partial");
    let capture_name = Path::new(&command[0])
        .file_name()
        .and_then(|name| name.to_str());
    if capture_name != Some(expected_capture_name.as_str()) {
        return Err(Gate0TraceError::InvalidSidecarField("command"));
    }

    let requested_samples = requested_samples.to_string();
    let expected = [
        "--driver",
        "dreamsourcelab-dslogic",
        "--channels",
        "0,1,2,3,4,5,6,7,8,9",
        "--config",
        "samplerate=2m:voltage_threshold=1.4-3.6:continuous=true",
        "--samples",
        requested_samples.as_str(),
        "--output-format",
        "binary",
        "--output-file",
        command[12].as_str(),
        "--loglevel",
        "4",
    ];
    let raw_name = raw_capture_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(Gate0TraceError::InvalidSidecarField("command"))?;
    let expected_raw_name = format!(".{raw_name}.partial");
    let command_raw_name = Path::new(&command[12])
        .file_name()
        .and_then(|name| name.to_str());
    if command[1..] != expected || command_raw_name != Some(expected_raw_name.as_str()) {
        return Err(Gate0TraceError::InvalidSidecarField("command"));
    }
    Ok(())
}
