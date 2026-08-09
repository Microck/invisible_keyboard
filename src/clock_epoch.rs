//! Session-bound controller clock epochs for deterministic two-hand merging.

use std::{
    collections::BTreeSet,
    error::Error,
    fmt,
    fs::{File, OpenOptions},
    io::{self, BufReader, Read, Seek, SeekFrom, Write},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

use crate::{
    clock::{
        ClockError, ClockFit, ClockFitReport, ClockSample, MAX_CLOCK_DRIFT_PPM, MIN_CLOCK_SAMPLES,
        MIN_CLOCK_SPAN_US, OnlineClockDiagnostics,
    },
    layout::Hand,
    manifest::{
        AcquisitionManifest, FrameIdentityError, ManifestError, ObservedTopologyError,
        format_sha256, parse_sha256, sha256,
    },
    protocol::{FLAG_CLOCK_UNSYNCED, FrameIdentity, SensorFrame},
    session::{
        CandidateDecode, FinalizedSession, RecordDecodeError, SessionAudit, SessionError,
        SessionHeader, audit_session_reader_with,
    },
};

pub const CLOCK_EPOCH_SET_SCHEMA: &str = "invisible-keyboard-clock-epochs-v1";
pub const MAX_CLOCK_EPOCH_SET_BYTES: usize = 65_536;
pub const LIVE_CLOCK_REPORT_SCHEMA: &str = "invisible-keyboard-live-clock-report-v2";
pub const MAX_LIVE_CLOCK_REPORT_BYTES: usize = 131_072;

/// Result of one strict streaming session audit and its independent offline
/// clock qualification.
///
/// A clock failure does not erase the successfully audited raw-session facts.
#[derive(Debug)]
pub struct StreamedClockQualification {
    pub session_audit: SessionAudit,
    pub clock_epochs: Result<ClockEpochSet, ClockEpochFitError>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ControllerClockEpoch {
    pub hand: Hand,
    pub controller_id: String,
    pub fit: ClockFit,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ClockEpochSet {
    pub schema: String,
    pub acquisition_manifest_sha256: String,
    pub session_id: [u8; 16],
    pub epochs: Vec<ControllerClockEpoch>,
}

/// Offline evidence authority for one immutable manifest/session pair.
/// Canonical exact fitting constructs this capability, with exact equality
/// required for supplied sidecars; operational causal fits never enter it.
pub struct QualifiedOfflineClocks<'a> {
    manifest: &'a AcquisitionManifest,
    session: &'a FinalizedSession,
    epochs: ClockEpochSet,
}

impl<'a> QualifiedOfflineClocks<'a> {
    pub(crate) fn manifest(&self) -> &'a AcquisitionManifest {
        self.manifest
    }

    pub(crate) fn session(&self) -> &'a FinalizedSession {
        self.session
    }

    pub(crate) fn epochs(&self) -> &ClockEpochSet {
        &self.epochs
    }

    pub(crate) fn into_epochs(self) -> ClockEpochSet {
        self.epochs
    }
}

impl ClockEpochSet {
    pub fn new(
        acquisition_manifest_sha256: [u8; 32],
        session_id: [u8; 16],
        epochs: Vec<ControllerClockEpoch>,
    ) -> Result<Self, ClockEpochError> {
        let value = Self {
            schema: CLOCK_EPOCH_SET_SCHEMA.to_owned(),
            acquisition_manifest_sha256: format_sha256(&acquisition_manifest_sha256),
            session_id,
            epochs,
        };
        value.validate()?;
        Ok(value)
    }

    pub fn from_json(source: &[u8]) -> Result<Self, ClockEpochError> {
        if source.len() > MAX_CLOCK_EPOCH_SET_BYTES {
            return Err(ClockEpochError::TooLarge {
                actual: source.len(),
            });
        }
        let value = serde_json::from_slice::<Self>(source)
            .map_err(|error| ClockEpochError::Json(error.to_string()))?;
        value.validate()?;
        Ok(value)
    }

    pub fn validate(&self) -> Result<(), ClockEpochError> {
        if self.schema != CLOCK_EPOCH_SET_SCHEMA {
            return Err(ClockEpochError::InvalidSchema);
        }
        if parse_sha256(&self.acquisition_manifest_sha256).is_none() {
            return Err(ClockEpochError::InvalidManifestHash);
        }
        if self.session_id == [0; 16] {
            return Err(ClockEpochError::InvalidSessionIdentity);
        }
        if self.epochs.is_empty() || self.epochs.len() > 2 {
            return Err(ClockEpochError::InvalidEpochCount(self.epochs.len()));
        }
        if self.epochs.len() == 2
            && (self.epochs[0].hand != Hand::Left || self.epochs[1].hand != Hand::Right)
        {
            return Err(ClockEpochError::InvalidHandOrder);
        }
        let mut controllers = BTreeSet::new();
        for epoch in &self.epochs {
            if epoch.controller_id.trim().is_empty() {
                return Err(ClockEpochError::EmptyControllerIdentity(epoch.hand));
            }
            if !controllers.insert(epoch.controller_id.as_str()) {
                return Err(ClockEpochError::DuplicateControllerIdentity);
            }
            epoch
                .fit
                .validate()
                .map_err(|error| ClockEpochError::Clock {
                    hand: epoch.hand,
                    error,
                })?;
        }
        Ok(())
    }

    /// Check declared identities only. Offline evidence consumers must use
    /// `qualify_session` to establish authority from preserved raw receipts.
    pub fn bind(
        &self,
        manifest: &AcquisitionManifest,
        header: &SessionHeader,
    ) -> Result<(), ClockEpochError> {
        self.validate()?;
        manifest.validate().map_err(ClockEpochError::Manifest)?;
        let manifest_hash = manifest.sha256().map_err(ClockEpochError::Manifest)?;
        if parse_sha256(&self.acquisition_manifest_sha256) != Some(manifest_hash)
            || header.manifest_sha256 != manifest_hash
        {
            return Err(ClockEpochError::ManifestIdentityMismatch);
        }
        if self.session_id != header.session_id {
            return Err(ClockEpochError::SessionIdentityMismatch);
        }
        if self.epochs.len() != manifest.hands.len() {
            return Err(ClockEpochError::TopologyMismatch);
        }
        for (epoch, hand) in self.epochs.iter().zip(&manifest.hands) {
            let expected_hand = Hand::from(hand.hand);
            if epoch.hand != expected_hand {
                return Err(ClockEpochError::TopologyMismatch);
            }
            if epoch.controller_id != hand.controller_id {
                return Err(ClockEpochError::ControllerIdentityMismatch(epoch.hand));
            }
            if hand.parsed_boot_id() != Some(epoch.fit.report().boot_id) {
                return Err(ClockEpochError::BootIdentityMismatch(epoch.hand));
            }
        }
        Ok(())
    }

    /// Admit supplied offline clocks only when every mapping parameter and
    /// report field equals the canonical bounded exact fit of this raw session.
    /// Retain the returned capability through nested consumers to avoid
    /// repeating the full-session qualification.
    pub fn qualify_session<'a>(
        &self,
        manifest: &'a AcquisitionManifest,
        session: &'a FinalizedSession,
    ) -> Result<QualifiedOfflineClocks<'a>, ClockEpochFitError> {
        self.bind(manifest, session.header())
            .map_err(ClockEpochFitError::Epoch)?;
        let qualified = Self::fit_qualified_session(manifest, session)?;
        let canonical = qualified.epochs();
        if self != canonical
            || self
                .epochs
                .iter()
                .zip(&canonical.epochs)
                .any(|(supplied, fitted)| {
                    let supplied = supplied.fit.report();
                    let fitted = fitted.fit.report();
                    supplied.slope_ns_per_us.to_bits() != fitted.slope_ns_per_us.to_bits()
                        || supplied.offset_ns_at_device_zero.to_bits()
                            != fitted.offset_ns_at_device_zero.to_bits()
                        || supplied.drift_ppm.to_bits() != fitted.drift_ppm.to_bits()
                })
        {
            return Err(ClockEpochFitError::NonCanonicalFit);
        }
        Ok(qualified)
    }

    /// Derive and retain canonical offline authority when there is no supplied
    /// sidecar to compare. Acquisition producers reuse this result rather than
    /// fitting the same immutable session again in their nested consumers.
    pub(crate) fn fit_qualified_session<'a>(
        manifest: &'a AcquisitionManifest,
        session: &'a FinalizedSession,
    ) -> Result<QualifiedOfflineClocks<'a>, ClockEpochFitError> {
        let epochs = Self::fit_session(manifest, session)?;
        Ok(QualifiedOfflineClocks {
            manifest,
            session,
            epochs,
        })
    }

    /// Fit every declared controller against preserved host receipt times.
    ///
    /// Controller timestamps are local monotonic evidence, not claims that the
    /// controller already shares the host clock. Each decoded frame supplies a
    /// device timestamp and its host receipt time. Nondecoded transport
    /// candidates remain in the session evidence but are not timestamp pairs.
    /// A controller-reported local clock fault rejects the complete fit.
    pub fn fit_session(
        manifest: &AcquisitionManifest,
        session: &FinalizedSession,
    ) -> Result<Self, ClockEpochFitError> {
        manifest.validate().map_err(ClockEpochFitError::Manifest)?;
        let qualification = Self::audit_and_fit_reader(manifest, session.bytes())
            .map_err(ClockEpochFitError::Session)?;
        if qualification.session_audit.sha256() != session.sha256()
            || qualification.session_audit.header() != session.header()
            || qualification.session_audit.summary() != session.summary()
            || qualification.session_audit.byte_len()
                != u64::try_from(session.bytes().len()).expect("session length fits u64")
        {
            return Err(ClockEpochFitError::SessionAuditMismatch);
        }
        qualification.clock_epochs
    }

    /// Strictly audit and exactly fit one file-backed session with bounded RAM.
    ///
    /// The canonical session is scanned once. Valid timestamp pairs are
    /// written to two temporary 16-byte/sample spools while that same pass
    /// hashes and accounts for the raw file. Each hand then needs one moment
    /// pass and four 16-bit radix passes over its compact spool to reproduce
    /// exact p50, p95, p99, and maximum residuals. At most three 65,536-entry
    /// `u64` histograms are resident, or 1,572,864 bytes.
    ///
    /// # Errors
    ///
    /// A session container/provenance failure is returned directly. Clock
    /// qualification failures remain inside [`StreamedClockQualification`] so
    /// callers can retain the independently audited raw-session facts.
    pub fn audit_and_fit_reader<R: Read>(
        manifest: &AcquisitionManifest,
        reader: R,
    ) -> Result<StreamedClockQualification, SessionError> {
        let setup = StreamingClockSetup::new(manifest);
        let mut setup = match setup {
            Ok(setup) => setup,
            Err(error) => {
                let session_audit = audit_session_reader_with(reader, |_, _, _| {})?;
                return Ok(StreamedClockQualification {
                    session_audit,
                    clock_epochs: Err(error),
                });
            }
        };
        let mut record_error = None;
        let session_audit = audit_session_reader_with(reader, |record_index, record, decode| {
            if record_error.is_some() {
                return;
            }
            let frame = match decode {
                CandidateDecode::NotDecoded => return,
                CandidateDecode::Decoded(frame) => frame,
                CandidateDecode::Invalid(error) => {
                    record_error = Some(ClockEpochFitError::InvalidDecodedRecord {
                        record_index,
                        error: RecordDecodeError::Frame(error),
                    });
                    return;
                }
            };
            if let Err(error) = validate_cached_frame_identity(&setup.identities, &frame) {
                record_error = Some(ClockEpochFitError::FrameIdentity {
                    record_index,
                    error,
                });
                return;
            }
            if frame.flags & FLAG_CLOCK_UNSYNCED != 0 {
                record_error = Some(ClockEpochFitError::UnreliableControllerClock {
                    record_index,
                    hand: frame.hand,
                });
                return;
            }
            let sample = ClockSample {
                device_time_us: frame.device_time_us,
                host_time_ns: record.host_time_ns,
            };
            let hand_index = setup
                .identities
                .iter()
                .position(|identity| identity.hand == frame.hand)
                .expect("validated frame identity names a declared hand");
            if let Err(error) = setup.spools[hand_index].push(sample) {
                record_error = Some(ClockEpochFitError::Io(error));
                return;
            }
            setup.moments[hand_index].observe(sample);
        })?;

        let clock_epochs = if session_audit.header().manifest_sha256 != setup.manifest_sha256 {
            Err(ClockEpochFitError::ManifestIdentityMismatch)
        } else if let Some(error) = record_error {
            Err(error)
        } else {
            fit_streaming_clock_setup(manifest, session_audit.header(), setup)
        };
        Ok(StreamedClockQualification {
            session_audit,
            clock_epochs,
        })
    }

    pub fn canonical_json(&self) -> Result<Vec<u8>, ClockEpochError> {
        self.validate()?;
        serde_json::to_vec(self).map_err(|error| ClockEpochError::Serialization(error.to_string()))
    }

    pub fn fingerprint(&self) -> Result<[u8; 32], ClockEpochError> {
        Ok(sha256(&self.canonical_json()?))
    }

    #[must_use]
    pub fn fit_for(&self, hand: Hand) -> Option<&ClockFit> {
        self.epochs
            .iter()
            .find(|epoch| epoch.hand == hand)
            .map(|epoch| &epoch.fit)
    }
}

struct StreamingClockSetup {
    manifest_sha256: [u8; 32],
    identities: Vec<FrameIdentity>,
    spools: Vec<ClockSampleSpool>,
    moments: Vec<StreamingClockMoments>,
}

impl StreamingClockSetup {
    fn new(manifest: &AcquisitionManifest) -> Result<Self, ClockEpochFitError> {
        manifest.validate().map_err(ClockEpochFitError::Manifest)?;
        let manifest_sha256 = manifest.sha256().map_err(ClockEpochFitError::Manifest)?;
        let identities = manifest
            .frame_identities()
            .map_err(ClockEpochFitError::Manifest)?;
        let mut spools = Vec::with_capacity(identities.len());
        for identity in &identities {
            spools.push(ClockSampleSpool::new(identity.hand).map_err(ClockEpochFitError::Io)?);
        }
        Ok(Self {
            manifest_sha256,
            moments: vec![StreamingClockMoments::default(); identities.len()],
            identities,
            spools,
        })
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct StreamingClockMoments {
    count: u64,
    first: Option<ClockSample>,
    last: Option<ClockSample>,
    first_order_error: Option<ClockError>,
    sum_device: f64,
    sum_host: f64,
}

impl StreamingClockMoments {
    fn observe(&mut self, sample: ClockSample) {
        let sample_index = usize::try_from(self.count).unwrap_or(usize::MAX);
        if self.first_order_error.is_none()
            && let Some(previous) = self.last
        {
            if sample.device_time_us <= previous.device_time_us {
                self.first_order_error = Some(ClockError::DeviceTimeNotIncreasing {
                    index: sample_index,
                });
            } else if sample.host_time_ns < previous.host_time_ns {
                self.first_order_error = Some(ClockError::HostTimeRegression {
                    index: sample_index,
                });
            }
        }
        self.first.get_or_insert(sample);
        self.last = Some(sample);
        self.count = self.count.saturating_add(1);
        self.sum_device += sample.device_time_us as f64;
        self.sum_host += sample.host_time_ns as f64;
    }

    fn preliminary(self, hand: Hand) -> Result<StreamingClockPreliminary, ClockEpochFitError> {
        let sample_count = usize::try_from(self.count)
            .map_err(|_| ClockEpochFitError::SampleCountOverflow { hand })?;
        if sample_count < MIN_CLOCK_SAMPLES {
            return Err(ClockEpochFitError::Clock {
                hand,
                error: ClockError::TooFewSamples {
                    actual: sample_count,
                },
            });
        }
        if let Some(error) = self.first_order_error {
            return Err(ClockEpochFitError::Clock { hand, error });
        }
        let first = self.first.expect("minimum sample count establishes first");
        let last = self.last.expect("minimum sample count establishes last");
        let span_us = last.device_time_us - first.device_time_us;
        if span_us < MIN_CLOCK_SPAN_US {
            return Err(ClockEpochFitError::Clock {
                hand,
                error: ClockError::SpanTooShort { actual_us: span_us },
            });
        }
        let count = sample_count as f64;
        Ok(StreamingClockPreliminary {
            sample_count,
            first,
            last,
            mean_device: self.sum_device / count,
            mean_host: self.sum_host / count,
        })
    }
}

#[derive(Clone, Copy)]
struct StreamingClockPreliminary {
    sample_count: usize,
    first: ClockSample,
    last: ClockSample,
    mean_device: f64,
    mean_host: f64,
}

#[derive(Clone, Copy)]
struct StreamingClockParameters {
    preliminary: StreamingClockPreliminary,
    slope_ns_per_us: f64,
    offset_ns_at_device_zero: f64,
}

fn fit_streaming_clock_setup(
    manifest: &AcquisitionManifest,
    header: &SessionHeader,
    mut setup: StreamingClockSetup,
) -> Result<ClockEpochSet, ClockEpochFitError> {
    let observed = |hand| {
        setup
            .identities
            .iter()
            .position(|identity| identity.hand == hand)
            .map_or(0, |index| setup.moments[index].count)
    };
    manifest
        .validate_observed_topology(observed(Hand::Left), observed(Hand::Right))
        .map_err(ClockEpochFitError::Topology)?;

    let mut epochs = Vec::with_capacity(manifest.hands.len());
    for (index, declared) in manifest.hands.iter().enumerate() {
        let hand = Hand::from(declared.hand);
        let boot_id = declared
            .parsed_boot_id()
            .expect("validated manifest has a boot identity");
        let fit = fit_streaming_hand(
            hand,
            boot_id,
            setup.moments[index],
            &mut setup.spools[index],
        )?;
        epochs.push(ControllerClockEpoch {
            hand,
            controller_id: declared.controller_id.clone(),
            fit,
        });
    }
    let set = ClockEpochSet::new(setup.manifest_sha256, header.session_id, epochs)
        .map_err(ClockEpochFitError::Epoch)?;
    set.bind(manifest, header)
        .map_err(ClockEpochFitError::Epoch)?;
    Ok(set)
}

fn fit_streaming_hand(
    hand: Hand,
    boot_id: [u8; 16],
    moments: StreamingClockMoments,
    spool: &mut ClockSampleSpool,
) -> Result<ClockFit, ClockEpochFitError> {
    let preliminary = moments.preliminary(hand)?;
    let mut covariance = 0.0_f64;
    let mut device_variance = 0.0_f64;
    spool
        .for_each(|sample| {
            let centered_device = sample.device_time_us as f64 - preliminary.mean_device;
            covariance += centered_device * (sample.host_time_ns as f64 - preliminary.mean_host);
            device_variance += centered_device * centered_device;
        })
        .map_err(ClockEpochFitError::Io)?;
    if !device_variance.is_finite() || device_variance <= 0.0 {
        return Err(ClockEpochFitError::Clock {
            hand,
            error: ClockError::NonFiniteFit,
        });
    }
    let slope_ns_per_us = covariance / device_variance;
    let offset_ns_at_device_zero =
        preliminary.mean_host - slope_ns_per_us * preliminary.mean_device;
    let drift_ppm = (slope_ns_per_us / 1_000.0 - 1.0) * 1_000_000.0;
    if !slope_ns_per_us.is_finite()
        || !offset_ns_at_device_zero.is_finite()
        || !drift_ppm.is_finite()
    {
        return Err(ClockEpochFitError::Clock {
            hand,
            error: ClockError::NonFiniteFit,
        });
    }
    if drift_ppm.abs() > MAX_CLOCK_DRIFT_PPM {
        return Err(ClockEpochFitError::Clock {
            hand,
            error: ClockError::DriftOutOfRange { drift_ppm },
        });
    }
    let parameters = StreamingClockParameters {
        preliminary,
        slope_ns_per_us,
        offset_ns_at_device_zero,
    };
    let residuals = exact_residual_summary(spool, parameters)?;
    let report = ClockFitReport {
        boot_id,
        sample_count: preliminary.sample_count,
        first_device_time_us: preliminary.first.device_time_us,
        last_device_time_us: preliminary.last.device_time_us,
        first_host_time_ns: preliminary.first.host_time_ns,
        last_host_time_ns: preliminary.last.host_time_ns,
        slope_ns_per_us,
        offset_ns_at_device_zero,
        drift_ppm,
        residual_p50_ns: residuals.percentiles[0],
        residual_p95_ns: residuals.percentiles[1],
        residual_p99_ns: residuals.percentiles[2],
        residual_max_ns: residuals.maximum,
    };
    ClockFit::from_report(report).map_err(|error| ClockEpochFitError::Clock { hand, error })
}

#[derive(Clone, Copy)]
struct ResidualSelection {
    prefix: u64,
    rank_within_prefix: u64,
}

struct ExactResidualSummary {
    percentiles: [u64; 3],
    maximum: u64,
}

fn exact_residual_summary(
    spool: &mut ClockSampleSpool,
    parameters: StreamingClockParameters,
) -> Result<ExactResidualSummary, ClockEpochFitError> {
    const RADIX_BITS: u32 = 16;
    const RADIX_BUCKETS: usize = 1 << RADIX_BITS;
    const PERCENTAGES: [usize; 3] = [50, 95, 99];

    let mut selections = PERCENTAGES.map(|percentage| {
        let numerator = (percentage as u128) * (parameters.preliminary.sample_count as u128);
        ResidualSelection {
            prefix: 0,
            rank_within_prefix: u64::try_from(numerator.div_ceil(100))
                .expect("a usize clock sample count produces a u64 percentile rank"),
        }
    });
    let mut maximum = 0_u64;
    for shift in [48_u32, 32, 16, 0] {
        let mut histograms = (0..selections.len())
            .map(|_| vec![0_u64; RADIX_BUCKETS])
            .collect::<Vec<_>>();
        spool
            .for_each(|sample| {
                let residual = clock_residual(parameters, sample);
                if shift == 48 {
                    maximum = maximum.max(residual);
                }
                let higher_mask = if shift == 48 {
                    0
                } else {
                    u64::MAX << (shift + RADIX_BITS)
                };
                for (selection, histogram) in selections.iter().zip(&mut histograms) {
                    if residual & higher_mask == selection.prefix & higher_mask {
                        let bucket = usize::try_from((residual >> shift) & 0xffff)
                            .expect("16-bit radix bucket fits usize");
                        histogram[bucket] = histogram[bucket].saturating_add(1);
                    }
                }
            })
            .map_err(ClockEpochFitError::Io)?;

        for (selection, histogram) in selections.iter_mut().zip(histograms) {
            let mut preceding = 0_u64;
            let mut chosen = None;
            for (bucket, count) in histogram.into_iter().enumerate() {
                if selection.rank_within_prefix <= preceding.saturating_add(count) {
                    chosen = Some(bucket);
                    break;
                }
                preceding = preceding.saturating_add(count);
            }
            let bucket = chosen.ok_or_else(|| {
                ClockEpochFitError::ClockConstruction(
                    "clock residual spool changed during exact percentile selection".to_owned(),
                )
            })?;
            selection.rank_within_prefix -= preceding;
            selection.prefix |= u64::try_from(bucket).expect("radix bucket fits u64") << shift;
        }
    }
    Ok(ExactResidualSummary {
        percentiles: selections.map(|selection| selection.prefix),
        maximum,
    })
}

fn clock_residual(parameters: StreamingClockParameters, sample: ClockSample) -> u64 {
    let predicted = parameters.offset_ns_at_device_zero
        + parameters.slope_ns_per_us * sample.device_time_us as f64;
    (predicted - sample.host_time_ns as f64)
        .abs()
        .round()
        .clamp(0.0, u64::MAX as f64) as u64
}

struct ClockSampleSpool {
    file: Option<File>,
    path: PathBuf,
    write_buffer: Vec<u8>,
}

impl ClockSampleSpool {
    const BUFFER_BYTES: usize = 64 * 1_024;

    fn new(hand: Hand) -> io::Result<Self> {
        static NEXT_SPOOL_ID: AtomicU64 = AtomicU64::new(0);

        let side = match hand {
            Hand::Left => "left",
            Hand::Right => "right",
        };
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let sequence = NEXT_SPOOL_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            ".invisible-keyboard-clock-{side}-{}-{timestamp}-{sequence}.spool",
            std::process::id()
        ));
        let mut options = OpenOptions::new();
        options.read(true).write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let file = options.open(&path)?;
        Ok(Self {
            file: Some(file),
            path,
            write_buffer: Vec::with_capacity(Self::BUFFER_BYTES),
        })
    }

    fn push(&mut self, sample: ClockSample) -> io::Result<()> {
        if self.write_buffer.len() + 16 > Self::BUFFER_BYTES {
            self.flush_write_buffer()?;
        }
        self.write_buffer
            .extend_from_slice(&sample.device_time_us.to_le_bytes());
        self.write_buffer
            .extend_from_slice(&sample.host_time_ns.to_le_bytes());
        Ok(())
    }

    fn for_each(&mut self, mut observe: impl FnMut(ClockSample)) -> io::Result<()> {
        self.flush_write_buffer()?;
        let file = self.file.as_mut().expect("live sample spool remains open");
        file.seek(SeekFrom::Start(0))?;
        let mut reader = BufReader::with_capacity(Self::BUFFER_BYTES, file);
        let mut encoded = [0_u8; 16];
        loop {
            let mut read = 0;
            while read < encoded.len() {
                match reader.read(&mut encoded[read..]) {
                    Ok(0) if read == 0 => return Ok(()),
                    Ok(0) => {
                        return Err(io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            "clock sample spool ended inside one sample",
                        ));
                    }
                    Ok(length) => read += length,
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    Err(error) => return Err(error),
                }
            }
            observe(ClockSample {
                device_time_us: u64::from_le_bytes(
                    encoded[..8].try_into().expect("eight device-time bytes"),
                ),
                host_time_ns: u64::from_le_bytes(
                    encoded[8..].try_into().expect("eight host-time bytes"),
                ),
            });
        }
    }

    fn flush_write_buffer(&mut self) -> io::Result<()> {
        if self.write_buffer.is_empty() {
            return Ok(());
        }
        self.file
            .as_mut()
            .expect("live sample spool remains open")
            .write_all(&self.write_buffer)?;
        self.write_buffer.clear();
        Ok(())
    }
}

// Normal returns, clock-fit errors, and unwinding all run `Drop`. SIGKILL or a
// power loss can leave an owner-only timestamp spool behind. An operator may
// delete such a file after confirming that no process with the encoded PID is
// alive; it contains only `(device_time_us, host_time_ns)` pairs.
impl Drop for ClockSampleSpool {
    fn drop(&mut self) {
        drop(self.file.take());
        let _ = std::fs::remove_file(&self.path);
    }
}

fn validate_cached_frame_identity(
    identities: &[FrameIdentity],
    frame: &SensorFrame,
) -> Result<(), FrameIdentityError> {
    let expected = identities
        .iter()
        .find(|identity| identity.hand == frame.hand)
        .ok_or(FrameIdentityError::UndeclaredHand(frame.hand))?;
    if frame.controller_id_sha256 != expected.controller_id_sha256 {
        return Err(FrameIdentityError::ControllerIdentityMismatch(frame.hand));
    }
    if frame.boot_id != expected.boot_id {
        return Err(FrameIdentityError::BootIdentityMismatch(frame.hand));
    }
    Ok(())
}

/// One recent operational fit. This is not a complete-session clock epoch.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OperationalControllerClock {
    pub hand: Hand,
    pub controller_id: String,
    pub fit: Option<ClockFit>,
    pub diagnostics: OnlineClockDiagnostics,
}

/// Session-bound live timing report with an explicit offline evidence result.
///
/// `operational_clocks` describe the recent causal fits that controlled live
/// output. Only `full_session_epoch`, when present, qualifies every decoded
/// receipt pair through the strict complete-session fitter.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LiveClockReport {
    pub schema: String,
    pub acquisition_manifest_sha256: String,
    pub session_id: [u8; 16],
    pub session_sha256: String,
    pub operational_clocks: Vec<OperationalControllerClock>,
    pub full_session_epoch: Option<ClockEpochSet>,
    pub offline_qualification_error: Option<String>,
}

impl LiveClockReport {
    pub fn new(
        session_audit: &SessionAudit,
        operational_clocks: Vec<OperationalControllerClock>,
        offline_qualification: Result<ClockEpochSet, String>,
    ) -> Result<Self, LiveClockReportError> {
        let (full_session_epoch, offline_qualification_error) = match offline_qualification {
            Ok(epoch) => (Some(epoch), None),
            Err(error) => (None, Some(error)),
        };
        let report = Self {
            schema: LIVE_CLOCK_REPORT_SCHEMA.to_owned(),
            acquisition_manifest_sha256: format_sha256(&session_audit.header().manifest_sha256),
            session_id: session_audit.header().session_id,
            session_sha256: format_sha256(&session_audit.sha256()),
            operational_clocks,
            full_session_epoch,
            offline_qualification_error,
        };
        report.validate()?;
        Ok(report)
    }

    pub fn from_json(source: &[u8]) -> Result<Self, LiveClockReportError> {
        if source.len() > MAX_LIVE_CLOCK_REPORT_BYTES {
            return Err(LiveClockReportError::TooLarge {
                actual: source.len(),
            });
        }
        let report = serde_json::from_slice::<Self>(source)
            .map_err(|error| LiveClockReportError::Json(error.to_string()))?;
        report.validate()?;
        Ok(report)
    }

    pub fn validate(&self) -> Result<(), LiveClockReportError> {
        if self.schema != LIVE_CLOCK_REPORT_SCHEMA {
            return Err(LiveClockReportError::InvalidSchema);
        }
        if parse_sha256(&self.acquisition_manifest_sha256).is_none() {
            return Err(LiveClockReportError::InvalidManifestHash);
        }
        if parse_sha256(&self.session_sha256).is_none() {
            return Err(LiveClockReportError::InvalidSessionHash);
        }
        if self.session_id == [0; 16] {
            return Err(LiveClockReportError::InvalidSessionIdentity);
        }
        if self.operational_clocks.is_empty() || self.operational_clocks.len() > 2 {
            return Err(LiveClockReportError::InvalidClockCount(
                self.operational_clocks.len(),
            ));
        }
        if self.operational_clocks.len() == 2
            && (self.operational_clocks[0].hand != Hand::Left
                || self.operational_clocks[1].hand != Hand::Right)
        {
            return Err(LiveClockReportError::InvalidHandOrder);
        }
        let mut controllers = BTreeSet::new();
        for clock in &self.operational_clocks {
            if clock.controller_id.trim().is_empty() {
                return Err(LiveClockReportError::EmptyControllerIdentity(clock.hand));
            }
            if !controllers.insert(clock.controller_id.as_str()) {
                return Err(LiveClockReportError::DuplicateControllerIdentity);
            }
            if let Some(fit) = clock.fit {
                fit.validate()
                    .map_err(|error| LiveClockReportError::Clock {
                        hand: clock.hand,
                        error,
                    })?;
            }
        }
        match (
            self.full_session_epoch.as_ref(),
            self.offline_qualification_error.as_deref(),
        ) {
            (Some(epoch), None) => {
                epoch
                    .validate()
                    .map_err(LiveClockReportError::OfflineEpoch)?;
                if epoch.acquisition_manifest_sha256 != self.acquisition_manifest_sha256
                    || epoch.session_id != self.session_id
                {
                    return Err(LiveClockReportError::OfflineIdentityMismatch);
                }
            }
            (None, Some(error)) if !error.trim().is_empty() => {}
            _ => return Err(LiveClockReportError::InvalidOfflineQualification),
        }
        Ok(())
    }

    pub fn bind(
        &self,
        manifest: &AcquisitionManifest,
        session_audit: &SessionAudit,
    ) -> Result<(), LiveClockReportError> {
        self.validate()?;
        manifest
            .validate()
            .map_err(LiveClockReportError::Manifest)?;
        let manifest_hash = manifest.sha256().map_err(LiveClockReportError::Manifest)?;
        let header = session_audit.header();
        if parse_sha256(&self.acquisition_manifest_sha256) != Some(manifest_hash)
            || header.manifest_sha256 != manifest_hash
        {
            return Err(LiveClockReportError::ManifestIdentityMismatch);
        }
        if self.session_id != header.session_id {
            return Err(LiveClockReportError::SessionIdentityMismatch);
        }
        if parse_sha256(&self.session_sha256) != Some(session_audit.sha256()) {
            return Err(LiveClockReportError::SessionContentMismatch);
        }
        if self.operational_clocks.len() != manifest.hands.len() {
            return Err(LiveClockReportError::TopologyMismatch);
        }
        for (clock, hand) in self.operational_clocks.iter().zip(&manifest.hands) {
            let expected_hand = Hand::from(hand.hand);
            if clock.hand != expected_hand {
                return Err(LiveClockReportError::TopologyMismatch);
            }
            if clock.controller_id != hand.controller_id {
                return Err(LiveClockReportError::ControllerIdentityMismatch(clock.hand));
            }
            if let Some(fit) = clock.fit
                && hand.parsed_boot_id() != Some(fit.report().boot_id)
            {
                return Err(LiveClockReportError::BootIdentityMismatch(clock.hand));
            }
        }
        if let Some(epoch) = &self.full_session_epoch {
            epoch
                .bind(manifest, header)
                .map_err(LiveClockReportError::OfflineEpoch)?;
        }
        Ok(())
    }

    pub fn canonical_json(&self) -> Result<Vec<u8>, LiveClockReportError> {
        self.validate()?;
        serde_json::to_vec(self)
            .map_err(|error| LiveClockReportError::Serialization(error.to_string()))
    }

    #[must_use]
    pub const fn offline_evidence_qualified(&self) -> bool {
        self.full_session_epoch.is_some()
    }

    #[must_use]
    pub fn operational_clocks_ready(&self) -> bool {
        self.operational_clocks
            .iter()
            .all(|clock| clock.fit.is_some())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum LiveClockReportError {
    TooLarge { actual: usize },
    Json(String),
    Serialization(String),
    InvalidSchema,
    InvalidManifestHash,
    InvalidSessionHash,
    InvalidSessionIdentity,
    InvalidClockCount(usize),
    InvalidHandOrder,
    EmptyControllerIdentity(Hand),
    DuplicateControllerIdentity,
    InvalidOfflineQualification,
    OfflineIdentityMismatch,
    Manifest(ManifestError),
    ManifestIdentityMismatch,
    SessionIdentityMismatch,
    SessionContentMismatch,
    TopologyMismatch,
    ControllerIdentityMismatch(Hand),
    BootIdentityMismatch(Hand),
    Clock { hand: Hand, error: ClockError },
    OfflineEpoch(ClockEpochError),
}

impl fmt::Display for LiveClockReportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge { actual } => write!(
                formatter,
                "live clock report is {actual} bytes; maximum is {MAX_LIVE_CLOCK_REPORT_BYTES}"
            ),
            Self::Json(error) => write!(formatter, "live clock report JSON is invalid: {error}"),
            Self::Serialization(error) => {
                write!(formatter, "live clock report serialization failed: {error}")
            }
            Self::InvalidSchema => formatter.write_str("live clock report schema is invalid"),
            Self::InvalidManifestHash => {
                formatter.write_str("live clock report manifest hash is invalid")
            }
            Self::InvalidSessionHash => {
                formatter.write_str("live clock report session hash is invalid")
            }
            Self::InvalidSessionIdentity => {
                formatter.write_str("live clock report session ID is zero")
            }
            Self::InvalidClockCount(actual) => write!(
                formatter,
                "live clock report has {actual} operational clocks; expected one or two"
            ),
            Self::InvalidHandOrder => {
                formatter.write_str("live operational clocks must order left before right")
            }
            Self::EmptyControllerIdentity(hand) => write!(
                formatter,
                "live operational clock {hand:?} controller ID is empty"
            ),
            Self::DuplicateControllerIdentity => {
                formatter.write_str("live operational clocks repeat a controller ID")
            }
            Self::InvalidOfflineQualification => formatter.write_str(
                "live clock report must contain exactly one offline epoch or qualification error",
            ),
            Self::OfflineIdentityMismatch => formatter.write_str(
                "full-session clock epoch does not share the live report manifest and session",
            ),
            Self::Manifest(error) => {
                write!(formatter, "live clock report manifest is invalid: {error}")
            }
            Self::ManifestIdentityMismatch => formatter
                .write_str("live clock report, manifest, and session do not share one manifest"),
            Self::SessionIdentityMismatch => {
                formatter.write_str("live clock report belongs to another session")
            }
            Self::SessionContentMismatch => {
                formatter.write_str("live clock report belongs to different session bytes")
            }
            Self::TopologyMismatch => {
                formatter.write_str("live clock report and manifest hand topology disagree")
            }
            Self::ControllerIdentityMismatch(hand) => write!(
                formatter,
                "live operational clock {hand:?} belongs to another controller"
            ),
            Self::BootIdentityMismatch(hand) => write!(
                formatter,
                "live operational clock {hand:?} belongs to another controller boot"
            ),
            Self::Clock { hand, error } => {
                write!(
                    formatter,
                    "live operational clock {hand:?} is invalid: {error}"
                )
            }
            Self::OfflineEpoch(error) => write!(
                formatter,
                "live clock report full-session epoch is invalid: {error}"
            ),
        }
    }
}

impl Error for LiveClockReportError {}

#[derive(Clone, Debug, PartialEq)]
pub enum ClockEpochError {
    TooLarge { actual: usize },
    Json(String),
    Serialization(String),
    InvalidSchema,
    InvalidManifestHash,
    InvalidSessionIdentity,
    InvalidEpochCount(usize),
    InvalidHandOrder,
    EmptyControllerIdentity(Hand),
    DuplicateControllerIdentity,
    Manifest(ManifestError),
    ManifestIdentityMismatch,
    SessionIdentityMismatch,
    TopologyMismatch,
    ControllerIdentityMismatch(Hand),
    BootIdentityMismatch(Hand),
    Clock { hand: Hand, error: ClockError },
}

impl fmt::Display for ClockEpochError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge { actual } => write!(
                formatter,
                "clock epoch set is {actual} bytes; maximum is {MAX_CLOCK_EPOCH_SET_BYTES}"
            ),
            Self::Json(error) => write!(formatter, "clock epoch JSON is invalid: {error}"),
            Self::Serialization(error) => {
                write!(formatter, "clock epoch serialization failed: {error}")
            }
            Self::InvalidSchema => formatter.write_str("clock epoch schema is invalid"),
            Self::InvalidManifestHash => {
                formatter.write_str("clock epoch manifest hash is invalid")
            }
            Self::InvalidSessionIdentity => formatter.write_str("clock epoch session ID is zero"),
            Self::InvalidEpochCount(actual) => {
                write!(
                    formatter,
                    "clock epoch set has {actual} hands; expected one or two"
                )
            }
            Self::InvalidHandOrder => {
                formatter.write_str("clock epochs must order left before right")
            }
            Self::EmptyControllerIdentity(hand) => {
                write!(formatter, "clock epoch {hand:?} controller ID is empty")
            }
            Self::DuplicateControllerIdentity => {
                formatter.write_str("clock epoch set repeats a controller ID")
            }
            Self::Manifest(error) => write!(formatter, "clock epoch manifest is invalid: {error}"),
            Self::ManifestIdentityMismatch => {
                formatter.write_str("clock epochs, manifest, and session do not share one manifest")
            }
            Self::SessionIdentityMismatch => {
                formatter.write_str("clock epochs belong to another session")
            }
            Self::TopologyMismatch => {
                formatter.write_str("clock epoch and manifest hand topology disagree")
            }
            Self::ControllerIdentityMismatch(hand) => {
                write!(
                    formatter,
                    "clock epoch {hand:?} belongs to another controller"
                )
            }
            Self::BootIdentityMismatch(hand) => {
                write!(
                    formatter,
                    "clock epoch {hand:?} belongs to another controller boot"
                )
            }
            Self::Clock { hand, error } => {
                write!(formatter, "clock epoch {hand:?} fit is invalid: {error}")
            }
        }
    }
}

impl Error for ClockEpochError {}

#[derive(Debug)]
pub enum ClockEpochFitError {
    Manifest(ManifestError),
    Topology(ObservedTopologyError),
    Session(SessionError),
    Io(io::Error),
    ManifestIdentityMismatch,
    SessionAuditMismatch,
    NonCanonicalFit,
    SampleCountOverflow {
        hand: Hand,
    },
    ClockConstruction(String),
    InvalidDecodedRecord {
        record_index: u64,
        error: RecordDecodeError,
    },
    FrameIdentity {
        record_index: u64,
        error: FrameIdentityError,
    },
    UnreliableControllerClock {
        record_index: u64,
        hand: Hand,
    },
    Clock {
        hand: Hand,
        error: ClockError,
    },
    Epoch(ClockEpochError),
}

impl fmt::Display for ClockEpochFitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Manifest(error) => write!(formatter, "clock-fit manifest is invalid: {error}"),
            Self::Topology(error) => write!(formatter, "clock-fit topology failed: {error}"),
            Self::Session(error) => write!(formatter, "clock-fit session is invalid: {error}"),
            Self::Io(error) => write!(formatter, "clock-fit bounded spool I/O failed: {error}"),
            Self::ManifestIdentityMismatch => {
                formatter.write_str("clock-fit session belongs to another manifest")
            }
            Self::SessionAuditMismatch => formatter
                .write_str("clock-fit streaming audit disagrees with finalized session evidence"),
            Self::NonCanonicalFit => formatter.write_str(
                "supplied offline clock epochs differ from the exact preserved raw-session fit",
            ),
            Self::SampleCountOverflow { hand } => {
                write!(formatter, "clock-fit {hand:?} sample count exceeds usize")
            }
            Self::ClockConstruction(error) => {
                write!(
                    formatter,
                    "clock-fit exact report construction failed: {error}"
                )
            }
            Self::InvalidDecodedRecord {
                record_index,
                error,
            } => write!(
                formatter,
                "clock-fit decoded record {record_index} is invalid: {error}"
            ),
            Self::FrameIdentity {
                record_index,
                error,
            } => write!(
                formatter,
                "clock-fit decoded record {record_index} has invalid wire identity: {error}"
            ),
            Self::UnreliableControllerClock { record_index, hand } => write!(
                formatter,
                "clock-fit record {record_index} for {hand:?} reports an invalid controller clock epoch"
            ),
            Self::Clock { hand, error } => {
                write!(formatter, "clock-fit {hand:?} epoch failed: {error}")
            }
            Self::Epoch(error) => write!(formatter, "clock-fit epoch set failed: {error}"),
        }
    }
}

impl Error for ClockEpochFitError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Manifest(error) => Some(error),
            Self::Topology(error) => Some(error),
            Self::Session(error) => Some(error),
            Self::Io(error) => Some(error),
            Self::Clock { error, .. } => Some(error),
            Self::Epoch(error) => Some(error),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamp_spool_is_private_and_removed_on_normal_drop() {
        #[cfg(unix)]
        use std::os::unix::fs::PermissionsExt;

        let spool = ClockSampleSpool::new(Hand::Left).expect("timestamp spool creates");
        let path = spool.path.clone();
        #[cfg(unix)]
        assert_eq!(
            std::fs::metadata(&path)
                .expect("timestamp spool metadata reads")
                .permissions()
                .mode()
                & 0o077,
            0,
            "group and other users cannot access a timestamp spool"
        );

        drop(spool);
        assert!(!path.exists(), "normal drop removes the timestamp spool");
    }

    #[test]
    fn timestamp_spool_is_removed_when_a_fit_path_returns_an_error() {
        let mut path = None;
        let failed = (|| -> io::Result<()> {
            let spool = ClockSampleSpool::new(Hand::Right)?;
            path = Some(spool.path.clone());
            Err(io::Error::other("fixture clock fit failed"))
        })();

        assert!(failed.is_err());
        assert!(
            !path.expect("error path records the spool path").exists(),
            "error return drops and removes the timestamp spool"
        );
    }
}
