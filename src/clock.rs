//! Fail-closed controller-to-host clock fitting and residual evidence.

use std::{collections::VecDeque, error::Error, fmt};

use serde::{Deserialize, Serialize};

/// Minimum pairs used by one fit.
pub const MIN_CLOCK_SAMPLES: usize = 8;

/// Minimum controller-time span used by one fit.
pub const MIN_CLOCK_SPAN_US: u64 = 100_000;

/// Provisional maximum absolute controller drift.
pub const MAX_CLOCK_DRIFT_PPM: f64 = 5_000.0;

/// Provisional maximum p95 absolute residual.
pub const MAX_CLOCK_RESIDUAL_P95_NS: u64 = 2_000_000;

/// Maximum time mapped beyond either edge of the observed interval.
pub const MAX_CLOCK_EXTRAPOLATION_US: u64 = 250_000;

/// Bounded rolling history for causal live clock fitting at about 200 Hz.
pub const MAX_ONLINE_CLOCK_SAMPLES: usize = 1_024;

/// Maximum initial device-time span allowed to recover from receipt jitter.
///
/// USB and FIFO reads can batch early frames under one delayed host receipt.
/// Output stays disarmed while later receipts dilute that startup distortion.
/// A clock that still violates a quality gate after this span fails closed.
pub const MAX_ONLINE_CLOCK_QUALITY_WARMUP_US: u64 = 500_000;

/// Device-time span retained in each causal online fit candidate.
///
/// The immutable session keeps every receipt pair. Limiting only the live fit
/// candidate prevents a delayed startup prefix from influencing operation for
/// the full 1,024-sample history.
pub const ONLINE_CLOCK_FIT_WINDOW_US: u64 = 250_000;

/// One controller/host timestamp pair retained from acquisition.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ClockSample {
    pub device_time_us: u64,
    pub host_time_ns: u64,
}

/// Inspectable quality summary for one immutable fit epoch.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ClockFitReport {
    pub boot_id: [u8; 16],
    pub sample_count: usize,
    pub first_device_time_us: u64,
    pub last_device_time_us: u64,
    pub first_host_time_ns: u64,
    pub last_host_time_ns: u64,
    pub slope_ns_per_us: f64,
    pub offset_ns_at_device_zero: f64,
    pub drift_ppm: f64,
    pub residual_p50_ns: u64,
    pub residual_p95_ns: u64,
    pub residual_p99_ns: u64,
    pub residual_max_ns: u64,
}

/// One affine mapping bound to a controller boot and fit interval.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ClockFit {
    report: ClockFitReport,
}

/// Inspectable counters for the causal online fitting policy.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OnlineClockDiagnostics {
    pub candidate_quality_failures: u64,
    pub holdover_frames: u64,
    pub accepted_fits: u64,
    pub maximum_holdover_age_us: u64,
}

impl ClockFit {
    /// Fit host nanoseconds as a linear function of controller microseconds.
    ///
    /// No sample is silently discarded. Poor drift or residuals reject the
    /// complete epoch so callers retain the raw timing evidence.
    ///
    /// # Errors
    ///
    /// Returns the first structural or quality-gate failure.
    pub fn fit(boot_id: [u8; 16], samples: &[ClockSample]) -> Result<Self, ClockError> {
        // Keep public fits allocation-free until all structural and drift gates
        // pass, matching the original error path. Online fits supply capacity.
        let mut residuals = Vec::new();
        Self::fit_with_residuals(boot_id, samples, &mut residuals)
    }

    fn fit_with_residuals(
        boot_id: [u8; 16],
        samples: &[ClockSample],
        residuals: &mut Vec<u64>,
    ) -> Result<Self, ClockError> {
        if samples.len() < MIN_CLOCK_SAMPLES {
            return Err(ClockError::TooFewSamples {
                actual: samples.len(),
            });
        }
        for (index, pair) in samples.windows(2).enumerate() {
            if pair[1].device_time_us <= pair[0].device_time_us {
                return Err(ClockError::DeviceTimeNotIncreasing { index: index + 1 });
            }
            if pair[1].host_time_ns < pair[0].host_time_ns {
                return Err(ClockError::HostTimeRegression { index: index + 1 });
            }
        }

        let first = samples[0];
        let last = *samples.last().expect("minimum sample count is nonzero");
        let span_us = last.device_time_us - first.device_time_us;
        if span_us < MIN_CLOCK_SPAN_US {
            return Err(ClockError::SpanTooShort { actual_us: span_us });
        }

        let count = samples.len() as f64;
        let mean_device = samples
            .iter()
            .map(|sample| sample.device_time_us as f64)
            .sum::<f64>()
            / count;
        let mean_host = samples
            .iter()
            .map(|sample| sample.host_time_ns as f64)
            .sum::<f64>()
            / count;
        let mut covariance = 0.0_f64;
        let mut device_variance = 0.0_f64;
        for sample in samples {
            let centered_device = sample.device_time_us as f64 - mean_device;
            covariance += centered_device * (sample.host_time_ns as f64 - mean_host);
            device_variance += centered_device * centered_device;
        }
        if !device_variance.is_finite() || device_variance <= 0.0 {
            return Err(ClockError::NonFiniteFit);
        }
        let slope_ns_per_us = covariance / device_variance;
        let offset_ns_at_device_zero = mean_host - slope_ns_per_us * mean_device;
        let drift_ppm = (slope_ns_per_us / 1_000.0 - 1.0) * 1_000_000.0;
        if !slope_ns_per_us.is_finite()
            || !offset_ns_at_device_zero.is_finite()
            || !drift_ppm.is_finite()
        {
            return Err(ClockError::NonFiniteFit);
        }
        if drift_ppm.abs() > MAX_CLOCK_DRIFT_PPM {
            return Err(ClockError::DriftOutOfRange { drift_ppm });
        }

        residuals.clear();
        residuals.extend(samples.iter().map(|sample| {
            let predicted =
                offset_ns_at_device_zero + slope_ns_per_us * sample.device_time_us as f64;
            (predicted - sample.host_time_ns as f64)
                .abs()
                .round()
                .clamp(0.0, u64::MAX as f64) as u64
        }));
        let residual_p50_ns = percentile(residuals, 50);
        let residual_p95_ns = percentile(residuals, 95);
        let residual_p99_ns = percentile(residuals, 99);
        let residual_max_ns = *residuals.iter().max().expect("residuals are nonempty");
        let report = ClockFitReport {
            boot_id,
            sample_count: samples.len(),
            first_device_time_us: first.device_time_us,
            last_device_time_us: last.device_time_us,
            first_host_time_ns: first.host_time_ns,
            last_host_time_ns: last.host_time_ns,
            slope_ns_per_us,
            offset_ns_at_device_zero,
            drift_ppm,
            residual_p50_ns,
            residual_p95_ns,
            residual_p99_ns,
            residual_max_ns,
        };
        if report.residual_p95_ns > MAX_CLOCK_RESIDUAL_P95_NS {
            return Err(ClockError::ResidualTooLarge {
                residual_p95_ns: report.residual_p95_ns,
            });
        }
        Ok(Self { report })
    }

    /// Build a shape-checked fit. Offline authority still requires qualification
    /// against the raw session.
    pub(crate) fn from_report(report: ClockFitReport) -> Result<Self, ClockError> {
        let fit = Self { report };
        fit.validate()?;
        Ok(fit)
    }

    /// Return the immutable fit evidence.
    #[must_use]
    pub const fn report(&self) -> &ClockFitReport {
        &self.report
    }

    /// Map one controller time into host monotonic nanoseconds.
    ///
    /// # Errors
    ///
    /// Rejects another boot, excessive extrapolation, or an unrepresentable
    /// result instead of silently joining incompatible epochs.
    pub fn map_device_time_ns(
        &self,
        boot_id: [u8; 16],
        device_time_us: u64,
    ) -> Result<u64, ClockError> {
        if boot_id != self.report.boot_id {
            return Err(ClockError::BootIdentityMismatch);
        }
        let below = self
            .report
            .first_device_time_us
            .saturating_sub(device_time_us);
        let above = device_time_us.saturating_sub(self.report.last_device_time_us);
        let extrapolation_us = below.max(above);
        if extrapolation_us > MAX_CLOCK_EXTRAPOLATION_US {
            return Err(ClockError::ExtrapolationTooLarge {
                actual_us: extrapolation_us,
            });
        }
        let mapped = self.report.offset_ns_at_device_zero
            + self.report.slope_ns_per_us * device_time_us as f64;
        if !mapped.is_finite() || mapped < 0.0 || mapped > u64::MAX as f64 {
            return Err(ClockError::MappedTimeOutOfRange);
        }
        Ok(mapped.round() as u64)
    }

    /// Validate a fit's shape and reported numeric gates.
    ///
    /// This does not establish offline evidence authority. Supplied session
    /// clock epochs must also pass the canonical raw-session qualification.
    ///
    /// # Errors
    ///
    /// Rejects nonfinite parameters and reports that violate v1 gates.
    pub fn validate(&self) -> Result<(), ClockError> {
        let report = self.report;
        if report.sample_count < MIN_CLOCK_SAMPLES {
            return Err(ClockError::TooFewSamples {
                actual: report.sample_count,
            });
        }
        let span_us = report
            .last_device_time_us
            .checked_sub(report.first_device_time_us)
            .ok_or(ClockError::DeviceTimeNotIncreasing { index: 0 })?;
        if span_us < MIN_CLOCK_SPAN_US {
            return Err(ClockError::SpanTooShort { actual_us: span_us });
        }
        if !report.slope_ns_per_us.is_finite()
            || !report.offset_ns_at_device_zero.is_finite()
            || !report.drift_ppm.is_finite()
        {
            return Err(ClockError::NonFiniteFit);
        }
        if report.drift_ppm.abs() > MAX_CLOCK_DRIFT_PPM {
            return Err(ClockError::DriftOutOfRange {
                drift_ppm: report.drift_ppm,
            });
        }
        if report.residual_p95_ns > MAX_CLOCK_RESIDUAL_P95_NS {
            return Err(ClockError::ResidualTooLarge {
                residual_p95_ns: report.residual_p95_ns,
            });
        }
        Ok(())
    }
}

/// Causal controller-to-host fitter that never uses future timestamp pairs.
#[derive(Clone)]
pub struct OnlineClockSynchronizer {
    boot_id: [u8; 16],
    samples: VecDeque<ClockSample>,
    fit_window_start: usize,
    fit_samples: Vec<ClockSample>,
    residuals: Vec<u64>,
    first_device_time_us: Option<u64>,
    current_fit: Option<ClockFit>,
    last_mapped_time_ns: Option<u64>,
    diagnostics: OnlineClockDiagnostics,
}

impl OnlineClockSynchronizer {
    #[must_use]
    pub fn new(boot_id: [u8; 16]) -> Self {
        Self {
            boot_id,
            samples: VecDeque::with_capacity(MAX_ONLINE_CLOCK_SAMPLES),
            fit_window_start: 0,
            fit_samples: Vec::with_capacity(MAX_ONLINE_CLOCK_SAMPLES),
            residuals: Vec::with_capacity(MAX_ONLINE_CLOCK_SAMPLES),
            first_device_time_us: None,
            current_fit: None,
            last_mapped_time_ns: None,
            diagnostics: OnlineClockDiagnostics::default(),
        }
    }

    /// Add one current receipt pair and map that same frame after the minimum
    /// evidence span exists. Before then, return `None` so output stays disarmed.
    pub fn observe(
        &mut self,
        boot_id: [u8; 16],
        device_time_us: u64,
        host_time_ns: u64,
    ) -> Result<Option<u64>, ClockError> {
        if boot_id != self.boot_id {
            return Err(ClockError::BootIdentityMismatch);
        }
        if let Some(previous) = self.samples.back() {
            if device_time_us <= previous.device_time_us {
                return Err(ClockError::DeviceTimeNotIncreasing {
                    index: self.samples.len(),
                });
            }
            if host_time_ns < previous.host_time_ns {
                return Err(ClockError::HostTimeRegression {
                    index: self.samples.len(),
                });
            }
        }
        self.first_device_time_us.get_or_insert(device_time_us);
        // Evict before insertion so the fixed-size rolling window never asks
        // VecDeque to grow beyond its configured capacity.
        if self.samples.len() == MAX_ONLINE_CLOCK_SAMPLES {
            self.samples.pop_front();
            self.fit_window_start = self.fit_window_start.saturating_sub(1);
        }
        self.samples.push_back(ClockSample {
            device_time_us,
            host_time_ns,
        });
        let latest_device_time_us = device_time_us;
        let first_device_time_us = self
            .first_device_time_us
            .expect("the first observation establishes boot time");
        let boot_span_us = latest_device_time_us - first_device_time_us;
        while latest_device_time_us - self.samples[self.fit_window_start].device_time_us
            > ONLINE_CLOCK_FIT_WINDOW_US
        {
            self.fit_window_start += 1;
        }
        self.fit_samples.clear();
        self.fit_samples
            .extend(self.samples.range(self.fit_window_start..).copied());
        let candidate =
            ClockFit::fit_with_residuals(self.boot_id, &self.fit_samples, &mut self.residuals);

        match candidate {
            Ok(fit) => {
                let mapped = fit.map_device_time_ns(self.boot_id, device_time_us)?;
                if self
                    .last_mapped_time_ns
                    .is_some_and(|previous| mapped < previous)
                {
                    return self.map_with_last_good(device_time_us);
                }
                self.current_fit = Some(fit);
                self.last_mapped_time_ns = Some(mapped);
                self.diagnostics.accepted_fits = self.diagnostics.accepted_fits.saturating_add(1);
                Ok(Some(mapped))
            }
            Err(
                error @ (ClockError::DriftOutOfRange { .. } | ClockError::ResidualTooLarge { .. }),
            ) => {
                self.diagnostics.candidate_quality_failures = self
                    .diagnostics
                    .candidate_quality_failures
                    .saturating_add(1);
                self.hold_or_wait(error, boot_span_us, device_time_us)
            }
            Err(error @ (ClockError::TooFewSamples { .. } | ClockError::SpanTooShort { .. })) => {
                self.hold_or_wait(error, boot_span_us, device_time_us)
            }
            Err(error) => Err(error),
        }
    }

    /// Explicitly disarm and begin a new controller boot epoch.
    pub fn reset(&mut self, boot_id: [u8; 16]) {
        self.boot_id = boot_id;
        self.samples.clear();
        self.fit_window_start = 0;
        self.fit_samples.clear();
        self.residuals.clear();
        self.first_device_time_us = None;
        self.current_fit = None;
        self.last_mapped_time_ns = None;
        self.diagnostics = OnlineClockDiagnostics::default();
    }

    #[must_use]
    pub const fn current_fit(&self) -> Option<&ClockFit> {
        self.current_fit.as_ref()
    }

    #[must_use]
    pub fn sample_count(&self) -> usize {
        self.samples.len()
    }

    #[must_use]
    pub const fn diagnostics(&self) -> OnlineClockDiagnostics {
        self.diagnostics
    }

    fn hold_or_wait(
        &mut self,
        candidate_error: ClockError,
        boot_span_us: u64,
        device_time_us: u64,
    ) -> Result<Option<u64>, ClockError> {
        if self.current_fit.is_some() {
            return self.map_with_last_good(device_time_us);
        }
        if boot_span_us < MAX_ONLINE_CLOCK_QUALITY_WARMUP_US {
            return Ok(None);
        }
        Err(candidate_error)
    }

    fn map_with_last_good(&mut self, device_time_us: u64) -> Result<Option<u64>, ClockError> {
        let fit = self
            .current_fit
            .expect("holdover requires one accepted fit");
        let holdover_age_us = device_time_us.saturating_sub(fit.report.last_device_time_us);
        let mapped = fit.map_device_time_ns(self.boot_id, device_time_us)?;
        if self
            .last_mapped_time_ns
            .is_some_and(|previous| mapped < previous)
        {
            return Err(ClockError::MappedTimeRegression);
        }
        self.last_mapped_time_ns = Some(mapped);
        self.diagnostics.holdover_frames = self.diagnostics.holdover_frames.saturating_add(1);
        self.diagnostics.maximum_holdover_age_us = self
            .diagnostics
            .maximum_holdover_age_us
            .max(holdover_age_us);
        Ok(Some(mapped))
    }
}

impl fmt::Debug for OnlineClockSynchronizer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OnlineClockSynchronizer")
            .field("boot_id", &self.boot_id)
            .field("samples", &self.samples)
            .field("first_device_time_us", &self.first_device_time_us)
            .field("current_fit", &self.current_fit)
            .field("last_mapped_time_ns", &self.last_mapped_time_ns)
            .field("diagnostics", &self.diagnostics)
            .finish()
    }
}

pub(crate) fn percentile(values: &mut [u64], percentage: usize) -> u64 {
    let rank = (percentage * values.len()).div_ceil(100);
    *values.select_nth_unstable(rank.saturating_sub(1)).1
}

/// Why a clock epoch cannot be fitted or mapped safely.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ClockError {
    TooFewSamples { actual: usize },
    SpanTooShort { actual_us: u64 },
    DeviceTimeNotIncreasing { index: usize },
    HostTimeRegression { index: usize },
    DriftOutOfRange { drift_ppm: f64 },
    ResidualTooLarge { residual_p95_ns: u64 },
    BootIdentityMismatch,
    ExtrapolationTooLarge { actual_us: u64 },
    MappedTimeOutOfRange,
    MappedTimeRegression,
    NonFiniteFit,
}

impl fmt::Display for ClockError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooFewSamples { actual } => write!(
                formatter,
                "clock fit has {actual} samples; expected at least {MIN_CLOCK_SAMPLES}"
            ),
            Self::SpanTooShort { actual_us } => write!(
                formatter,
                "clock fit spans {actual_us} us; expected at least {MIN_CLOCK_SPAN_US} us"
            ),
            Self::DeviceTimeNotIncreasing { index } => {
                write!(
                    formatter,
                    "controller time does not increase at sample {index}"
                )
            }
            Self::HostTimeRegression { index } => {
                write!(formatter, "host time regresses at sample {index}")
            }
            Self::DriftOutOfRange { drift_ppm } => write!(
                formatter,
                "clock drift {drift_ppm:.3} ppm exceeds {MAX_CLOCK_DRIFT_PPM:.0} ppm"
            ),
            Self::ResidualTooLarge { residual_p95_ns } => write!(
                formatter,
                "clock p95 residual {residual_p95_ns} ns exceeds {MAX_CLOCK_RESIDUAL_P95_NS} ns"
            ),
            Self::BootIdentityMismatch => {
                formatter.write_str("clock fit belongs to another controller boot")
            }
            Self::ExtrapolationTooLarge { actual_us } => write!(
                formatter,
                "clock mapping extrapolates {actual_us} us; maximum is {MAX_CLOCK_EXTRAPOLATION_US} us"
            ),
            Self::MappedTimeOutOfRange => {
                formatter.write_str("mapped host time is outside u64 nanoseconds")
            }
            Self::MappedTimeRegression => {
                formatter.write_str("online clock replacement regresses mapped host time")
            }
            Self::NonFiniteFit => formatter.write_str("clock fit is nonfinite"),
        }
    }
}

impl Error for ClockError {}
