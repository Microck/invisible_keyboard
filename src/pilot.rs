//! Non-armable one-hand purchase screening against independently archived truth.
//!
//! Signal thresholds are fitted only to initial neutral. Truth is used solely
//! for coverage validation and scoring, never to choose or reset detections.

use std::{
    collections::VecDeque,
    error::Error,
    fmt,
    io::{self, Read},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    calibration::SETUP_TIME_CAP_SECONDS,
    decoder::{BorrowedTapWindow, MAX_INTER_SENSOR_SKEW_US, causal_contact_energies_borrowed},
    layout::{Finger, Hand, PhysicalPosition, all_slots, slot_for_position},
    manifest::{
        AcquisitionManifest, FrameCanonicalizer, format_id128, format_sha256, parse_sha256,
    },
    protocol::{ALL_SENSORS_VALID, FLAG_ACQUISITION_OVERRUN, FLAG_CLOCK_UNSYNCED, SensorFrame},
    session::{FinalizedSession, SessionRecordKind, validate_calibration_session_size},
};

pub const PILOT_TRUTH_SCHEMA: &str = "invisible-keyboard-one-hand-pilot-truth-v1";
pub const PILOT_REPORT_SCHEMA: &str = "invisible-keyboard-one-hand-pilot-report-v1";
pub const MAX_PILOT_TRUTH_BYTES: usize = 1_048_576;
const WINDOW_FRAMES: usize = 16;
const MATCH_US: u64 = 200_000;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PilotTruthSource {
    HighSpeedVideo,
    ContactSwitch,
    InstrumentedKeyboard,
    OpticalTracker,
}

/// Half-open controller-microsecond interval [start_us, end_us).
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PilotInterval {
    pub start_us: u64,
    pub end_us: u64,
}

impl PilotInterval {
    fn contains(self, time: u64) -> bool {
        self.start_us <= time && time < self.end_us
    }

    fn duration(self) -> Option<u64> {
        self.end_us
            .checked_sub(self.start_us)
            .filter(|duration| *duration > 0)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PilotContact {
    pub time_us: u64,
    pub position: PhysicalPosition,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PilotNegativeKind {
    MouseUse,
    PhoneHandling,
    Scratching,
    Gesturing,
    ObjectHandling,
    HandRepositioning,
    QuietRest,
}

const NEGATIVE_KINDS: [PilotNegativeKind; 7] = [
    PilotNegativeKind::MouseUse,
    PilotNegativeKind::PhoneHandling,
    PilotNegativeKind::Scratching,
    PilotNegativeKind::Gesturing,
    PilotNegativeKind::ObjectHandling,
    PilotNegativeKind::HandRepositioning,
    PilotNegativeKind::QuietRest,
];

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PilotNegativeInterval {
    pub kind: PilotNegativeKind,
    pub interval: PilotInterval,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PilotTruth {
    pub schema: String,
    pub session_sha256: String,
    pub manifest_sha256: String,
    pub session_id: String,
    pub hand: Hand,
    pub source: PilotTruthSource,
    pub truth_artifact_sha256: String,
    pub clock_mapping_sha256: String,
    pub maximum_alignment_error_us: u32,
    pub neutral: PilotInterval,
    pub observation: PilotInterval,
    pub contacts: Vec<PilotContact>,
    pub negatives: Vec<PilotNegativeInterval>,
}

impl PilotTruth {
    /// Parse bounded, typed annotation. Session binding is checked by reduction.
    pub fn from_json(bytes: &[u8]) -> Result<Self, PilotError> {
        if bytes.len() > MAX_PILOT_TRUTH_BYTES {
            return Err(PilotError::Invalid("pilot truth exceeds the 1 MiB bound"));
        }
        let truth: Self = serde_json::from_slice(bytes).map_err(PilotError::Json)?;
        truth.validate()?;
        Ok(truth)
    }

    fn validate(&self) -> Result<(), PilotError> {
        if self.schema != PILOT_TRUTH_SCHEMA || self.maximum_alignment_error_us > 5_000 {
            return Err(PilotError::Invalid(
                "unsupported truth schema or alignment error exceeds 5 ms",
            ));
        }
        if self
            .neutral
            .duration()
            .is_none_or(|duration| duration < 3_000_000)
            || self.observation.duration().is_none()
            || self.neutral.end_us != self.observation.start_us
            || self
                .observation
                .end_us
                .checked_sub(self.neutral.start_us)
                .is_none_or(|duration| duration > u64::from(SETUP_TIME_CAP_SECONDS) * 1_000_000)
        {
            return Err(PilotError::Invalid(
                "neutral and observation must be contiguous, cover at least 3 seconds neutral, and fit the 1200-second bound",
            ));
        }
        for hash in [
            &self.session_sha256,
            &self.manifest_sha256,
            &self.truth_artifact_sha256,
            &self.clock_mapping_sha256,
        ] {
            if parse_sha256(hash).is_none() {
                return Err(PilotError::Invalid(
                    "truth identities must be lowercase SHA-256 hex",
                ));
            }
        }
        let mut previous = None;
        for contact in &self.contacts {
            if contact.position.hand != self.hand
                || slot_for_position(contact.position).is_none()
                || !self.observation.contains(contact.time_us)
                || contact
                    .time_us
                    .checked_add(MATCH_US)
                    .is_none_or(|end| !self.observation.contains(end))
                || previous.is_some_and(|time: u64| contact.time_us.saturating_sub(time) < 500_000)
            {
                return Err(PilotError::Invalid(
                    "contacts must name valid one-hand positions, be ordered 500 ms apart, and have complete 200 ms scoring windows",
                ));
            }
            previous = Some(contact.time_us);
        }
        for slot in all_slots()
            .iter()
            .filter(|slot| slot.position.hand == self.hand)
        {
            if self
                .contacts
                .iter()
                .filter(|contact| contact.position == slot.position)
                .count()
                < 10
            {
                return Err(PilotError::Invalid(
                    "every physical position, including reserved positions, requires at least ten independent contacts",
                ));
            }
        }
        if self.negatives.len() != NEGATIVE_KINDS.len() {
            return Err(PilotError::Invalid(
                "exactly seven named negative intervals are required",
            ));
        }
        for kind in NEGATIVE_KINDS {
            if self
                .negatives
                .iter()
                .filter(|negative| negative.kind == kind)
                .count()
                != 1
            {
                return Err(PilotError::Invalid(
                    "each negative activity must occur exactly once",
                ));
            }
        }
        for (index, negative) in self.negatives.iter().enumerate() {
            let interval = negative.interval;
            if interval
                .duration()
                .is_none_or(|duration| duration < 60_000_000)
                || interval.start_us < self.observation.start_us
                || interval.end_us > self.observation.end_us
                || self
                    .contacts
                    .iter()
                    .any(|contact| interval.contains(contact.time_us))
                || self.negatives[..index].iter().any(|other| {
                    interval.start_us < other.interval.end_us
                        && other.interval.start_us < interval.end_us
                })
            {
                return Err(PilotError::Invalid(
                    "negative intervals must be covered, at least 60 seconds, disjoint, and free of contacts",
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug)]
pub enum PilotError {
    Invalid(&'static str),
    Evidence(String),
    Io(io::Error),
    Json(serde_json::Error),
}

impl fmt::Display for PilotError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(message) => formatter.write_str(message),
            Self::Evidence(message) => formatter.write_str(message),
            Self::Io(error) => error.fmt(formatter),
            Self::Json(error) => error.fmt(formatter),
        }
    }
}

impl Error for PilotError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::Invalid(_) | Self::Evidence(_) => None,
        }
    }
}

fn evidence(error: impl fmt::Display) -> PilotError {
    PilotError::Evidence(error.to_string())
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PilotDisposition {
    SupportsTwoHandBuild,
    Stop,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PilotEvent {
    pub time_us: u64,
    pub finger: Finger,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PilotFingerOutcome {
    pub finger: Finger,
    pub neutral_max_energy: u64,
    pub low_threshold: u64,
    pub high_threshold: u64,
    /// Median peak causal energy in independent contact scoring windows.
    pub median_contact_energy: f64,
    /// Energy ratio uses max(1, neutral_max_energy) as the noise floor.
    pub median_signal_noise_energy_ratio: f64,
    pub saturated_frames: u64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PilotPositionOutcome {
    pub position: PhysicalPosition,
    pub contacts: usize,
    pub correct: usize,
    pub recall: f64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PilotNegativeOutcome {
    pub kind: PilotNegativeKind,
    pub interval: PilotInterval,
    pub extra_events: usize,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PilotVariantOutcome {
    pub contacts: usize,
    pub correct: usize,
    /// Contacts with neither a correct nor a wrong-finger event in their window.
    pub missing: usize,
    /// Contacts with a wrong-finger event and no correct match. Those events
    /// also count as extras because they did not match the required finger.
    pub wrong_finger: usize,
    pub extra_events: usize,
    pub events: Vec<PilotEvent>,
    pub fingers: [PilotFingerOutcome; 5],
    pub positions: Vec<PilotPositionOutcome>,
    pub negatives: Vec<PilotNegativeOutcome>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PilotReport {
    pub schema: String,
    pub evidence_level: String,
    pub operator_limitation: String,
    pub disposition: PilotDisposition,
    pub truth: PilotTruth,
    pub frames: u64,
    pub bad_frames: u64,
    pub saturated_frames: u64,
    pub dorsal_saturated_frames: u64,
    pub raw: PilotVariantOutcome,
    pub corrected: PilotVariantOutcome,
}

impl PilotReport {
    pub fn canonical_json(&self) -> Result<Vec<u8>, PilotError> {
        serde_json::to_vec(self).map_err(PilotError::Json)
    }
}

/// Hash complete referenced artifacts with fixed memory; nonempty files are
/// required, but software cannot establish whether their contents are honest.
fn artifact_digest(mut reader: impl Read) -> Result<String, PilotError> {
    let mut hash = Sha256::new();
    let mut buffer = [0_u8; 65_536];
    let mut nonempty = false;
    loop {
        let count = match reader.read(&mut buffer) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            result => result.map_err(PilotError::Io)?,
        };
        if count == 0 {
            break;
        }
        nonempty = true;
        hash.update(&buffer[..count]);
    }
    if !nonempty {
        return Err(PilotError::Invalid(
            "referenced truth and clock mapping must be nonempty",
        ));
    }
    Ok(format_sha256(&hash.finalize().into()))
}

/// Detector state is deliberately ignorant of truth, position prompts, and
/// activity boundaries. Only neutral energies set its frozen thresholds.
#[derive(Default)]
struct Detector {
    neutral_max: [u64; 5],
    candidate: Option<usize>,
    low_run: u8,
    latched: bool,
    events: Vec<PilotEvent>,
}

impl Detector {
    fn observe_neutral(&mut self, energies: [u64; 5]) {
        for (maximum, energy) in self.neutral_max.iter_mut().zip(energies) {
            *maximum = (*maximum).max(energy);
        }
    }

    fn low(&self, finger: usize) -> u64 {
        self.neutral_max[finger].saturating_mul(2).max(1_024)
    }
    fn high(&self, finger: usize) -> u64 {
        self.neutral_max[finger].saturating_mul(8).max(4_096)
    }

    fn observe(&mut self, time_us: u64, energies: [u64; 5]) {
        if self.latched {
            if energies
                .iter()
                .enumerate()
                .all(|(finger, energy)| *energy <= self.low(finger))
            {
                self.low_run += 1;
                if self.low_run == 4 {
                    self.latched = false;
                    self.low_run = 0;
                }
            } else {
                self.low_run = 0;
            }
            return;
        }
        let strongest = (0..5)
            .max_by_key(|finger| energies[*finger])
            .expect("five fingers");
        let second = (0..5)
            .filter(|finger| *finger != strongest)
            .map(|finger| energies[finger])
            .max()
            .unwrap_or(0);
        if energies[strongest] >= self.high(strongest)
            && u128::from(energies[strongest]) * 2 >= u128::from(second) * 3
        {
            if self.candidate == Some(strongest) {
                self.events.push(PilotEvent {
                    time_us,
                    finger: Finger::from_sensor_index(strongest)
                        .expect("five finger sensor indices"),
                });
                self.latched = true;
                self.candidate = None;
            } else {
                self.candidate = Some(strongest);
            }
        } else {
            self.candidate = None;
        }
    }
}

/// Scoring can see truth; detection cannot. This accumulator never feeds back
/// into either detector, and retains only one peak per annotated contact.
struct ContactSignals {
    next: usize,
    raw: Vec<u64>,
    corrected: Vec<u64>,
}

impl ContactSignals {
    fn new(count: usize) -> Self {
        Self {
            next: 0,
            raw: vec![0; count],
            corrected: vec![0; count],
        }
    }

    fn observe(&mut self, truth: &PilotTruth, time: u64, raw: [u64; 5], corrected: [u64; 5]) {
        while self.next < truth.contacts.len()
            && truth.contacts[self.next].time_us + MATCH_US < time
        {
            self.next += 1;
        }
        if let Some(contact) = truth
            .contacts
            .get(self.next)
            .filter(|contact| contact.time_us <= time)
        {
            let finger = contact.position.finger.sensor_index();
            self.raw[self.next] = self.raw[self.next].max(raw[finger]);
            self.corrected[self.next] = self.corrected[self.next].max(corrected[finger]);
        }
    }
}

/// Reduce a strict ordinary one-hand session without creating any calibration
/// or live admission. Callers must load the manifest with its rig and measured
/// axis evidence; the normal evidence-bound manifest loader is not bypassed.
/// Actual truth-source and clock-mapping bytes are hashed, not merely labels.
pub fn reduce_pilot<T: Read, M: Read>(
    manifest: &AcquisitionManifest,
    session: &FinalizedSession,
    truth: &PilotTruth,
    truth_artifact: T,
    clock_mapping: M,
) -> Result<PilotReport, PilotError> {
    truth.validate()?;
    manifest.validate().map_err(evidence)?;
    validate_calibration_session_size(session.bytes().len() as u64).map_err(evidence)?;
    if manifest.hands.len() != 1 {
        return Err(PilotError::Invalid(
            "pilot requires exactly one declared hand",
        ));
    }
    let summary = session.summary();
    manifest
        .validate_observed_topology(summary.left.received, summary.right.received)
        .map_err(evidence)?;
    if summary.invalid_records != 0
        || summary.quiesce_requests != 0
        || summary.quiesce_receipts != 0
    {
        return Err(PilotError::Invalid(
            "pilot accepts only ordinary decoded sensor records",
        ));
    }
    let stream = if truth.hand == Hand::Left {
        &summary.left
    } else {
        &summary.right
    };
    if stream.received == 0
        || stream.missing != 0
        || stream.duplicates != 0
        || stream.reordered != 0
        || stream.device_time_regressions != 0
        || stream.host_time_regressions != 0
    {
        return Err(PilotError::Invalid(
            "pilot stream must be one-hand, contiguous, and monotonic",
        ));
    }
    let manifest_hash = manifest.sha256().map_err(evidence)?;
    if session.header().manifest_sha256 != manifest_hash
        || session.header().nominal_period_us != manifest.nominal_period_us
        || truth.session_sha256 != format_sha256(&session.sha256())
        || truth.manifest_sha256 != format_sha256(&manifest_hash)
        || truth.session_id != format_id128(&session.header().session_id)
    {
        return Err(PilotError::Invalid(
            "pilot truth, session, and manifest identities do not match",
        ));
    }
    if artifact_digest(truth_artifact)? != truth.truth_artifact_sha256
        || artifact_digest(clock_mapping)? != truth.clock_mapping_sha256
    {
        return Err(PilotError::Invalid(
            "referenced truth or clock-mapping bytes do not match their digest",
        ));
    }

    let canonicalizer = FrameCanonicalizer::new(manifest).map_err(evidence)?;
    let mut reader = session.reader();
    let mut window: VecDeque<SensorFrame> = VecDeque::with_capacity(WINDOW_FRAMES);
    let mut raw = Detector::default();
    let mut corrected = Detector::default();
    let mut signals = ContactSignals::new(truth.contacts.len());
    let mut previous: Option<u64> = None;
    let mut frames = 0;
    let mut bad_frames = 0;
    let mut saturated_frames = 0;
    let mut saturation = [0_u64; 6];
    let period = u64::from(manifest.nominal_period_us);
    while let Some(record) = reader.next_record().map_err(evidence)? {
        if record.kind != SessionRecordKind::Decoded {
            return Err(PilotError::Invalid(
                "pilot accepts only ordinary sensor records",
            ));
        }
        let wire = record.decode().map_err(evidence)?;
        if wire.hand != truth.hand {
            return Err(PilotError::Invalid(
                "truth hand does not match the recorded hand",
            ));
        }
        let time = wire.device_time_us;
        if previous.is_none() && time != truth.neutral.start_us {
            return Err(PilotError::Invalid(
                "neutral must begin at the first recorded frame",
            ));
        }
        if previous.is_some_and(|prior| time <= prior || time - prior > period * 2) {
            return Err(PilotError::Invalid(
                "pilot has a duplicate time or a gap exceeding two nominal periods",
            ));
        }
        if time >= truth.observation.end_us {
            return Err(PilotError::Invalid(
                "observation excludes retained post-neutral samples",
            ));
        }
        previous = Some(time);
        frames += 1;
        let mut saturated = false;
        for (index, sensor) in wire.sensors.iter().enumerate() {
            if sensor
                .acceleration
                .iter()
                .chain(&sensor.angular_velocity)
                .any(|value| *value == i16::MIN || *value == i16::MAX)
            {
                saturation[index] += 1;
                saturated = true;
            }
        }
        saturated_frames += u64::from(saturated);
        let min_offset = wire
            .sensors
            .iter()
            .map(|sensor| i32::from(sensor.sample_time_offset_us))
            .min()
            .unwrap_or(0);
        let max_offset = wire
            .sensors
            .iter()
            .map(|sensor| i32::from(sensor.sample_time_offset_us))
            .max()
            .unwrap_or(0);
        if wire.valid_sensor_mask != ALL_SENSORS_VALID
            || wire.flags & (FLAG_CLOCK_UNSYNCED | FLAG_ACQUISITION_OVERRUN) != 0
            || max_offset - min_offset > MAX_INTER_SENSOR_SKEW_US
        {
            bad_frames += 1;
        }
        let frame = canonicalizer.canonicalize(&wire).map_err(evidence)?;
        if window.len() == WINDOW_FRAMES {
            window.pop_front();
        }
        window.push_back(frame);
        let (first, second) = window.as_slices();
        let borrowed = BorrowedTapWindow::new(first, second).map_err(evidence)?;
        if window.len() < WINDOW_FRAMES {
            continue;
        }
        let raw_energy = causal_contact_energies_borrowed(borrowed, false);
        let corrected_energy = causal_contact_energies_borrowed(borrowed, true);
        if time < truth.neutral.end_us {
            raw.observe_neutral(raw_energy);
            corrected.observe_neutral(corrected_energy);
        } else {
            raw.observe(time, raw_energy);
            corrected.observe(time, corrected_energy);
            signals.observe(truth, time, raw_energy, corrected_energy);
        }
    }
    if previous.and_then(|time| time.checked_add(period)) != Some(truth.observation.end_us) {
        return Err(PilotError::Invalid(
            "observation must end one nominal period after the final frame",
        ));
    }
    let raw = score(raw, signals.raw, truth, saturation);
    let corrected = score(corrected, signals.corrected, truth, saturation);
    let supports = bad_frames == 0
        && saturated_frames == 0
        && corrected.extra_events == 0
        && corrected.correct * 10 >= corrected.contacts * 9
        && corrected
            .positions
            .iter()
            .all(|position| position.correct * 5 >= position.contacts * 4)
        && corrected
            .fingers
            .iter()
            .all(|finger| finger.median_signal_noise_energy_ratio >= 4.0);
    Ok(PilotReport {
        schema: PILOT_REPORT_SCHEMA.to_owned(),
        evidence_level: "replay-tested".to_owned(),
        operator_limitation: "Hashes bind supplied bytes, not physical truth. The operator is responsible for independent, complete contact annotation and measured clock synchronization. This provisional engineering screen is not hardware validation, Gate 0B, calibration, or output permission.".to_owned(),
        disposition: if supports { PilotDisposition::SupportsTwoHandBuild } else { PilotDisposition::Stop },
        truth: truth.clone(), frames, bad_frames, saturated_frames,
        dorsal_saturated_frames: saturation[5], raw, corrected,
    })
}

fn score(
    detector: Detector,
    signals: Vec<u64>,
    truth: &PilotTruth,
    saturation: [u64; 6],
) -> PilotVariantOutcome {
    let mut correct = vec![false; truth.contacts.len()];
    let mut wrong = vec![false; truth.contacts.len()];
    let mut extras = 0;
    let mut negatives: Vec<_> = truth
        .negatives
        .iter()
        .map(|negative| PilotNegativeOutcome {
            kind: negative.kind,
            interval: negative.interval,
            extra_events: 0,
        })
        .collect();
    let mut next = 0;
    for event in &detector.events {
        while next < truth.contacts.len() && truth.contacts[next].time_us + MATCH_US < event.time_us
        {
            next += 1;
        }
        let candidate = truth
            .contacts
            .get(next)
            .filter(|contact| contact.time_us <= event.time_us);
        if candidate.is_some_and(|contact| contact.position.finger == event.finger)
            && !correct[next]
        {
            correct[next] = true;
        } else {
            extras += 1;
            if candidate.is_some_and(|contact| contact.position.finger != event.finger) {
                wrong[next] = true;
            }
            for negative in &mut negatives {
                if negative.interval.contains(event.time_us) {
                    negative.extra_events += 1;
                }
            }
        }
    }
    let fingers = std::array::from_fn(|index| {
        let finger = Finger::from_sensor_index(index).expect("five finger sensor indices");
        let mut energies: Vec<u64> = truth
            .contacts
            .iter()
            .zip(&signals)
            .filter_map(|(contact, signal)| (contact.position.finger == finger).then_some(*signal))
            .collect();
        energies.sort_unstable();
        let middle = energies.len() / 2;
        let median = if energies.len().is_multiple_of(2) {
            (energies[middle - 1] as f64 + energies[middle] as f64) / 2.0
        } else {
            energies[middle] as f64
        };
        PilotFingerOutcome {
            finger,
            neutral_max_energy: detector.neutral_max[index],
            low_threshold: detector.low(index),
            high_threshold: detector.high(index),
            median_contact_energy: median,
            median_signal_noise_energy_ratio: median / detector.neutral_max[index].max(1) as f64,
            saturated_frames: saturation[index],
        }
    });
    let positions = all_slots()
        .iter()
        .filter(|slot| slot.position.hand == truth.hand)
        .map(|slot| {
            let mut contacts = 0;
            let mut matched = 0;
            for (index, contact) in truth.contacts.iter().enumerate() {
                if contact.position == slot.position {
                    contacts += 1;
                    matched += usize::from(correct[index]);
                }
            }
            PilotPositionOutcome {
                position: slot.position,
                contacts,
                correct: matched,
                recall: matched as f64 / contacts as f64,
            }
        })
        .collect();
    PilotVariantOutcome {
        contacts: truth.contacts.len(),
        correct: correct.iter().filter(|matched| **matched).count(),
        missing: correct
            .iter()
            .zip(&wrong)
            .filter(|(correct, wrong)| !**correct && !**wrong)
            .count(),
        wrong_finger: correct
            .iter()
            .zip(&wrong)
            .filter(|(correct, wrong)| !**correct && **wrong)
            .count(),
        extra_events: extras,
        events: detector.events,
        fingers,
        positions,
        negatives,
    }
}
