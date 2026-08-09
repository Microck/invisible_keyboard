//! Versioned acquisition provenance and canonical manifest hashing.

use std::{collections::BTreeSet, error::Error, fmt};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub use crate::{layout::CORE28_LAYOUT_ID, protocol::PROTOCOL_VERSION};
use crate::{
    layout::Hand,
    protocol::{FrameIdentity, SENSOR_COUNT, SensorFrame},
};

/// Maximum accepted source manifest size before JSON parsing.
pub const MAX_MANIFEST_BYTES: usize = 65_536;

/// Maximum accepted physical-rig evidence size before JSON parsing.
pub const MAX_RIG_EVIDENCE_BYTES: usize = 65_536;

/// Maximum accepted axis-characterization report size before JSON parsing.
pub const MAX_AXIS_CHARACTERIZATION_BYTES: usize = 65_536;

/// Current acquisition-manifest schema discriminator.
pub const ACQUISITION_MANIFEST_SCHEMA: &str = "invisible-keyboard-acquisition-manifest-v4";

/// Pre-capture schema whose all-zero boot IDs are replaced from wire evidence.
pub const ACQUISITION_TEMPLATE_SCHEMA: &str = "invisible-keyboard-acquisition-template-v4";

/// Immutable as-built rig evidence schema.
pub const RIG_EVIDENCE_SCHEMA: &str = "invisible-keyboard-rig-evidence-v1";

/// Immutable six-face axis-characterization report schema.
pub const AXIS_CHARACTERIZATION_SCHEMA: &str = "invisible-keyboard-axis-characterization-v1";

/// The one supported measurement method, shared by reduction and admission.
pub const AXIS_CHARACTERIZATION_METHOD_ID: &str = "fixed-order-600-stationary-iks3-v1";

/// Report fields describe this fixed profile; they cannot choose pass limits.
pub const AXIS_CHARACTERIZATION_CRITERIA: AxisCharacterizationCriteria =
    AxisCharacterizationCriteria {
        expected_gravity_lsb: 8192,
        maximum_component_residual_lsb: 820,
        minimum_samples_per_face: 600,
    };

/// Canonical projection hashed before an axis report exists.
pub const AXIS_CHARACTERIZATION_CANDIDATE_SCHEMA: &str =
    "invisible-keyboard-axis-characterization-candidate-v1";

/// Right-handed canonical carrier frame used by every characterized sensor.
///
/// +X points distally and +Z points dorsally. +Y is chosen so X cross Y = Z,
/// which makes +Y radial on the right hand and ulnar on the left hand.
pub const CANONICAL_AXIS_CONVENTION: &str = "x-distal-z-dorsal-right-handed-y-by-hand-v1";

/// One physical hand represented by an acquisition controller.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ManifestHand {
    Left,
    Right,
}

impl From<ManifestHand> for Hand {
    fn from(hand: ManifestHand) -> Self {
        match hand {
            ManifestHand::Left => Self::Left,
            ManifestHand::Right => Self::Right,
        }
    }
}

/// Anatomical role of one sensor in the canonical frame order.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SensorRole {
    Thumb,
    Index,
    Middle,
    Ring,
    Pinky,
    Dorsal,
}

/// State of one physical check in an immutable rig evidence record.
///
/// A not-yet-run or failed record remains useful provenance, but it cannot
/// authorize an acquisition manifest.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum EvidenceStatus {
    NotYetRun,
    Failed,
    Passed,
}

/// Common status and exact artifact identity for one physical check.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceFact {
    pub status: EvidenceStatus,
    pub artifact_sha256: Option<String>,
}

impl EvidenceFact {
    fn validate(&self, field: &'static str) -> Result<(), EvidenceArtifactError> {
        match (&self.status, &self.artifact_sha256) {
            (EvidenceStatus::NotYetRun, None) => Ok(()),
            (EvidenceStatus::Failed | EvidenceStatus::Passed, Some(hash)) => {
                require_evidence_sha256(hash, field)
            }
            _ => Err(EvidenceArtifactError::InconsistentField(field)),
        }
    }

    const fn was_run(&self) -> bool {
        !matches!(self.status, EvidenceStatus::NotYetRun)
    }
}

/// Immutable as-built identity and measured mass for one mounted sensor.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RigSensorEvidence {
    pub sensor_index: u8,
    pub mux_channel: u8,
    pub role: SensorRole,
    pub mount_id: String,
    pub wiring_id: String,
    pub interrupt_pin: String,
    pub mass_milligrams: Option<u32>,
}

/// Immutable as-built identity and mass evidence for one wearable hand.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RigHandEvidence {
    pub hand: ManifestHand,
    pub assembly_id: String,
    pub controller_id: String,
    pub total_mass_milligrams: Option<u32>,
    pub mass_measurement: EvidenceFact,
    pub sensors: [RigSensorEvidence; SENSOR_COUNT],
}

/// Continuity coverage for the completed wiring harness.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RigContinuityEvidence {
    pub fact: EvidenceFact,
    pub checked_path_count: Option<u16>,
}

/// Photo coverage for the exact as-built rig.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RigPhotoEvidence {
    pub fact: EvidenceFact,
    pub photo_count: u8,
}

/// Measured supply and current facts for the complete wearable rig.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RigPowerEvidence {
    pub fact: EvidenceFact,
    pub supply_millivolts: Option<u32>,
    pub idle_current_microamps: Option<u32>,
    pub peak_current_microamps: Option<u32>,
}

/// Logic-analyzer capture facts for the exact wired rig.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RigAnalyzerEvidence {
    pub fact: EvidenceFact,
    pub sample_rate_hz: Option<u32>,
    pub capture_duration_milliseconds: Option<u32>,
}

/// Passive surface used for the physical evidence run.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RigSurfaceEvidence {
    pub surface_id: String,
    pub material: String,
    pub thickness_micrometers: Option<u32>,
    pub fact: EvidenceFact,
}

/// Typing posture fixed for the physical evidence run.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RigPostureEvidence {
    pub posture_id: String,
    pub description: String,
    pub fact: EvidenceFact,
}

/// Independent device used to establish contact truth.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RigTruthDeviceEvidence {
    pub device_id: String,
    pub model: String,
    pub capture_method: String,
    pub fact: EvidenceFact,
}

/// Measured sensor-to-fixture alignment quality.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RigAlignmentEvidence {
    pub fixture_id: String,
    pub maximum_error_millidegrees: Option<u32>,
    pub fact: EvidenceFact,
}

/// Immutable physical evidence for one exact as-built rig and test context.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RigEvidenceRecord {
    pub schema: String,
    pub record_id: String,
    pub recorded_at_utc: String,
    pub operator_id: String,
    pub controller_board: String,
    pub sensor_model: String,
    pub hands: Vec<RigHandEvidence>,
    pub continuity: RigContinuityEvidence,
    pub photos: RigPhotoEvidence,
    pub power: RigPowerEvidence,
    pub analyzer: RigAnalyzerEvidence,
    pub surface: RigSurfaceEvidence,
    pub posture: RigPostureEvidence,
    pub truth_device: RigTruthDeviceEvidence,
    pub alignment: RigAlignmentEvidence,
}

/// Canonical fixture face order used by every axis report.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum AxisFace {
    PositiveX,
    NegativeX,
    PositiveY,
    NegativeY,
    PositiveZ,
    NegativeZ,
}

/// Anatomical direction of canonical +Y for one explicitly identified hand.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PositiveYDirection {
    Radial,
    Ulnar,
}

pub(crate) const AXIS_FACE_ORDER: [AxisFace; 6] = [
    AxisFace::PositiveX,
    AxisFace::NegativeX,
    AxisFace::PositiveY,
    AxisFace::NegativeY,
    AxisFace::PositiveZ,
    AxisFace::NegativeZ,
];

/// One raw accelerometer mean from one fixed fixture face.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AxisFaceMean {
    pub face: AxisFace,
    pub sample_count: u32,
    pub mean_acceleration_lsb: [i32; 3],
}

/// Explicit pass criteria shared by every sensor in one report.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AxisCharacterizationCriteria {
    pub expected_gravity_lsb: u32,
    pub maximum_component_residual_lsb: u32,
    pub minimum_samples_per_face: u32,
}

/// Six-face evidence and selected signed rotation for one sensor.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SensorAxisCharacterization {
    pub sensor_index: u8,
    pub role: SensorRole,
    pub raw_face_means: [AxisFaceMean; 6],
    pub axis_map: [i8; 9],
    pub observed_maximum_component_residual_lsb: u32,
    pub passed: bool,
}

/// Axis evidence for one controller hand.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HandAxisCharacterization {
    pub hand: ManifestHand,
    pub positive_y_direction: PositiveYDirection,
    pub sensors: [SensorAxisCharacterization; SENSOR_COUNT],
}

/// Immutable axis report derived from a separate raw characterization session.
///
/// Parsing or passing this report does not authorize calibration or live use.
/// Authorization requires a manifest whose physical candidate projection,
/// rig evidence hash, report hash, topology, and axis maps all bind exactly.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AxisCharacterizationReport {
    pub schema: String,
    pub report_id: String,
    pub completed_at_utc: String,
    pub characterizer_build: String,
    pub method_id: String,
    pub coordinate_convention: String,
    pub candidate_manifest_sha256: String,
    pub source_session_sha256: String,
    pub rig_evidence_sha256: String,
    pub criteria: AxisCharacterizationCriteria,
    pub hands: Vec<HandAxisCharacterization>,
    pub passed: bool,
}

/// One exact sensor-register write in the canonical acquisition profile.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SensorRegisterSetting {
    pub address: u8,
    pub value: u8,
}

/// Register settings and matching manifest declarations for current captures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AcquisitionProfile {
    pub ctrl1_xl: SensorRegisterSetting,
    pub ctrl2_g: SensorRegisterSetting,
    pub sensor_odr_hz: u16,
    pub accelerometer_range_g: u16,
    pub gyroscope_range_dps: u16,
    pub i2c_bus_hz: u32,
    pub nominal_period_us: u32,
    pub sensor_order: [SensorRole; SENSOR_COUNT],
}

/// The only acquisition profile accepted by the current manifest schema.
///
/// `CTRL1_XL=0x58` declares 208 Hz and +/-4 g. `CTRL2_G=0x58`
/// declares 208 Hz and +/-1000 dps. Keeping the register bytes beside their
/// manifest values makes a range change one explicit schema-contract change.
pub const CANONICAL_ACQUISITION_PROFILE: AcquisitionProfile = AcquisitionProfile {
    ctrl1_xl: SensorRegisterSetting {
        address: 0x10,
        value: 0x58,
    },
    ctrl2_g: SensorRegisterSetting {
        address: 0x11,
        value: 0x58,
    },
    sensor_odr_hz: 208,
    accelerometer_range_g: 4,
    gyroscope_range_dps: 1_000,
    i2c_bus_hz: 400_000,
    nominal_period_us: 5_000,
    sensor_order: [
        SensorRole::Thumb,
        SensorRole::Index,
        SensorRole::Middle,
        SensorRole::Ring,
        SensorRole::Pinky,
        SensorRole::Dorsal,
    ],
};

pub(crate) const CANONICAL_SENSOR_ROLES: [SensorRole; SENSOR_COUNT] =
    CANONICAL_ACQUISITION_PROFILE.sensor_order;

const PLACEHOLDER_IDENTITY_PREFIX: &str = "replace-with-";
const PLACEHOLDER_IDENTITIES: [&str; 8] = [
    "placeholder",
    "todo",
    "tbd",
    "unknown",
    "unset",
    "independent-firmware-unvalidated-v1",
    "independent-host-v1",
    "independent-host-bazel-v1",
];

/// Immutable physical provenance for one independently mounted IMU.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SensorManifest {
    /// Canonical frame index, independent of physical wiring order.
    pub sensor_index: u8,
    pub mux_channel: u8,
    pub role: SensorRole,
    pub mount_id: String,
    pub wiring_id: String,
    /// Stable controller pin identity for the sensor's direct data-ready edge.
    pub interrupt_pin: String,
    /// Row-major proper signed rotation from sensor to canonical axes.
    pub axis_map: [i8; 9],
}

/// Controller and per-sensor provenance for one physical hand.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HandManifest {
    pub hand: ManifestHand,
    pub controller_id: String,
    /// Controller-generated boot identity captured during the handshake.
    pub boot_id: String,
    /// Fixed canonical order: thumb through pinky, then dorsal reference.
    pub sensors: [SensorManifest; SENSOR_COUNT],
}

impl HandManifest {
    #[must_use]
    pub fn parsed_boot_id(&self) -> Option<[u8; 16]> {
        parse_id128(&self.boot_id)
    }

    /// Compute the exact identity that firmware must put in each frame.
    #[must_use]
    pub fn frame_identity(&self) -> Option<FrameIdentity> {
        Some(FrameIdentity {
            hand: Hand::from(self.hand),
            controller_id_sha256: sha256(self.controller_id.as_bytes()),
            boot_id: self.parsed_boot_id()?,
        })
    }
}

/// Complete immutable settings and provenance for one capture.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AcquisitionManifest {
    pub schema: String,
    pub layout_id: String,
    pub protocol_version: u8,
    pub hands: Vec<HandManifest>,
    pub controller_board: String,
    pub firmware_build: String,
    pub host_build: String,
    pub sensor_model: String,
    pub sensors_per_hand: u8,
    pub sensor_odr_hz: u16,
    pub accelerometer_range_g: u16,
    pub gyroscope_range_dps: u16,
    pub filter_profile: String,
    pub i2c_bus_hz: u32,
    pub nominal_period_us: u32,
    pub prompt_corpus_sha256: String,
    pub study_id: String,
    pub split_id: String,
    pub participant_alias: String,
    pub consent_record: String,
    pub retention_policy: String,
    /// Canonical SHA-256 identity of the exact as-built rig evidence record.
    pub rig_evidence_sha256: String,
    /// Canonical SHA-256 identity of the passed six-face axis report.
    pub axis_characterization_sha256: String,
    pub calibration_artifact_sha256: Option<String>,
    /// Held-out readiness evidence required before any runtime can emit keys.
    pub calibration_validation_sha256: Option<String>,
}

/// Validated pre-capture settings awaiting controller-generated boot IDs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AcquisitionManifestTemplate {
    pending: AcquisitionManifest,
}

/// Cached per-hand identity and axis rotations for one validated manifest.
///
/// Wire frames remain raw evidence. Consumers that interpret sensor vectors
/// pass each frame through this boundary exactly once before fitting, replay,
/// validation, or live decoding.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrameCanonicalizer {
    hands: Vec<CanonicalHand>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CanonicalHand {
    identity: FrameIdentity,
    axis_maps: [[i8; 9]; SENSOR_COUNT],
}

impl FrameCanonicalizer {
    /// Cache the exact wire identities and axis rotations from one manifest.
    pub fn new(manifest: &AcquisitionManifest) -> Result<Self, ManifestError> {
        manifest.validate()?;
        let hands = manifest
            .hands
            .iter()
            .map(|hand| CanonicalHand {
                identity: hand
                    .frame_identity()
                    .expect("validated manifest hand has a wire identity"),
                axis_maps: hand.sensors.each_ref().map(|sensor| sensor.axis_map),
            })
            .collect();
        Ok(Self { hands })
    }

    /// Validate one frame's cached identity and rotate every raw sensor vector
    /// into the canonical anatomical axes. Metadata and sample timing remain
    /// byte-for-byte equivalent to the decoded wire value.
    pub fn canonicalize(
        &self,
        frame: &SensorFrame,
    ) -> Result<SensorFrame, FrameCanonicalizationError> {
        let hand = self
            .hands
            .iter()
            .find(|candidate| candidate.identity.hand == frame.hand)
            .ok_or(FrameCanonicalizationError::Identity(
                FrameIdentityError::UndeclaredHand(frame.hand),
            ))?;
        if frame.controller_id_sha256 != hand.identity.controller_id_sha256 {
            return Err(FrameCanonicalizationError::Identity(
                FrameIdentityError::ControllerIdentityMismatch(frame.hand),
            ));
        }
        if frame.boot_id != hand.identity.boot_id {
            return Err(FrameCanonicalizationError::Identity(
                FrameIdentityError::BootIdentityMismatch(frame.hand),
            ));
        }

        let mut canonical = *frame;
        for (sensor_index, (reading, axis_map)) in
            canonical.sensors.iter_mut().zip(hand.axis_maps).enumerate()
        {
            reading.acceleration = rotate_raw_vector(
                axis_map,
                reading.acceleration,
                frame.hand,
                sensor_index,
                SensorVector::Acceleration,
            )?;
            reading.angular_velocity = rotate_raw_vector(
                axis_map,
                reading.angular_velocity,
                frame.hand,
                sensor_index,
                SensorVector::AngularVelocity,
            )?;
        }
        Ok(canonical)
    }
}

/// Raw sensor vector whose signed rotation cannot be represented as `i16`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SensorVector {
    Acceleration,
    AngularVelocity,
}

/// Why a raw wire frame cannot enter the canonical sensor coordinate system.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FrameCanonicalizationError {
    Identity(FrameIdentityError),
    AxisOverflow {
        hand: Hand,
        sensor_index: usize,
        vector: SensorVector,
        axis: usize,
    },
}

fn rotate_raw_vector(
    axis_map: [i8; 9],
    vector: [i16; 3],
    hand: Hand,
    sensor_index: usize,
    vector_kind: SensorVector,
) -> Result<[i16; 3], FrameCanonicalizationError> {
    let mut rotated = [0_i16; 3];
    for (row, output) in rotated.iter_mut().enumerate() {
        let value = (0..3)
            .map(|column| i32::from(axis_map[row * 3 + column]) * i32::from(vector[column]))
            .sum::<i32>();
        *output = i16::try_from(value).map_err(|_| FrameCanonicalizationError::AxisOverflow {
            hand,
            sensor_index,
            vector: vector_kind,
            axis: row,
        })?;
    }
    Ok(rotated)
}

impl RigEvidenceRecord {
    /// Parse and validate one bounded physical-rig evidence record.
    ///
    /// A valid record may contain explicit not-yet-run or failed checks. Such
    /// a record remains non-authorizing until every required check has passed.
    pub fn from_json(source: &[u8]) -> Result<Self, EvidenceArtifactError> {
        if source.len() > MAX_RIG_EVIDENCE_BYTES {
            return Err(EvidenceArtifactError::TooLarge {
                artifact: "rig evidence",
                actual: source.len(),
                maximum: MAX_RIG_EVIDENCE_BYTES,
            });
        }
        let record = serde_json::from_slice::<Self>(source).map_err(|error| {
            EvidenceArtifactError::Json {
                artifact: "rig evidence",
                message: error.to_string(),
            }
        })?;
        record.validate()?;
        Ok(record)
    }

    /// Validate record structure and measurement/status consistency.
    pub fn validate(&self) -> Result<(), EvidenceArtifactError> {
        if self.schema != RIG_EVIDENCE_SCHEMA {
            return Err(EvidenceArtifactError::InvalidSchema("rig evidence"));
        }
        for (value, field) in [
            (&self.record_id, "record_id"),
            (&self.recorded_at_utc, "recorded_at_utc"),
            (&self.operator_id, "operator_id"),
            (&self.controller_board, "controller_board"),
            (&self.sensor_model, "sensor_model"),
            (&self.surface.surface_id, "surface.surface_id"),
            (&self.surface.material, "surface.material"),
            (&self.posture.posture_id, "posture.posture_id"),
            (&self.posture.description, "posture.description"),
            (&self.truth_device.device_id, "truth_device.device_id"),
            (&self.truth_device.model, "truth_device.model"),
            (
                &self.truth_device.capture_method,
                "truth_device.capture_method",
            ),
            (&self.alignment.fixture_id, "alignment.fixture_id"),
        ] {
            require_evidence_identity(value, field)?;
        }
        validate_hand_count_and_order(self.hands.iter().map(|hand| hand.hand), "rig evidence")?;

        let mut assembly_ids = BTreeSet::new();
        let mut controller_ids = BTreeSet::new();
        let mut mount_ids = BTreeSet::new();
        let mut wiring_ids = BTreeSet::new();
        for (hand_index, hand) in self.hands.iter().enumerate() {
            require_evidence_identity(&hand.assembly_id, "hands.assembly_id")?;
            require_evidence_identity(&hand.controller_id, "hands.controller_id")?;
            if !assembly_ids.insert(hand.assembly_id.as_str()) {
                return Err(EvidenceArtifactError::DuplicateIdentity(
                    "hands.assembly_id",
                ));
            }
            if !controller_ids.insert(hand.controller_id.as_str()) {
                return Err(EvidenceArtifactError::DuplicateIdentity(
                    "hands.controller_id",
                ));
            }
            hand.mass_measurement.validate("hands.mass_measurement")?;

            let mass_was_measured = hand.mass_measurement.was_run();
            match hand.total_mass_milligrams {
                None => {
                    if mass_was_measured {
                        return Err(EvidenceArtifactError::InconsistentField(
                            "hands.total_mass_milligrams",
                        ));
                    }
                    if hand
                        .sensors
                        .iter()
                        .any(|sensor| sensor.mass_milligrams.is_some())
                    {
                        return Err(EvidenceArtifactError::InconsistentField(
                            "hands.sensors.mass_milligrams",
                        ));
                    }
                }
                Some(0) => {
                    return Err(EvidenceArtifactError::InconsistentField(
                        "hands.total_mass_milligrams",
                    ));
                }
                Some(_) if !mass_was_measured => {
                    return Err(EvidenceArtifactError::InconsistentField(
                        "hands.total_mass_milligrams",
                    ));
                }
                Some(_) => {}
            }
            validate_rig_sensor_identities(hand_index, hand, &mut mount_ids, &mut wiring_ids)?;
            if let Some(total_mass) = hand.total_mass_milligrams {
                let sensor_mass = hand.sensors.iter().try_fold(0_u32, |sum, sensor| {
                    let mass = sensor.mass_milligrams.filter(|mass| *mass > 0).ok_or(
                        EvidenceArtifactError::InconsistentField("hands.sensors.mass_milligrams"),
                    )?;
                    sum.checked_add(mass)
                        .ok_or(EvidenceArtifactError::InvalidField(
                            "hands.sensors.mass_milligrams",
                        ))
                })?;
                if total_mass < sensor_mass {
                    return Err(EvidenceArtifactError::InvalidField(
                        "hands.total_mass_milligrams",
                    ));
                }
            }
        }

        self.continuity.fact.validate("continuity")?;
        validate_optional_positive_measurement(
            self.continuity.fact.was_run(),
            self.continuity.checked_path_count.map(u32::from),
            "continuity.checked_path_count",
        )?;

        self.photos.fact.validate("photos")?;
        if self.photos.fact.was_run() != (self.photos.photo_count > 0) {
            return Err(EvidenceArtifactError::InconsistentField(
                "photos.photo_count",
            ));
        }

        self.power.fact.validate("power")?;
        for (measurement, field) in [
            (self.power.supply_millivolts, "power.supply_millivolts"),
            (
                self.power.idle_current_microamps,
                "power.idle_current_microamps",
            ),
            (
                self.power.peak_current_microamps,
                "power.peak_current_microamps",
            ),
        ] {
            validate_optional_positive_measurement(self.power.fact.was_run(), measurement, field)?;
        }
        if self
            .power
            .idle_current_microamps
            .zip(self.power.peak_current_microamps)
            .is_some_and(|(idle, peak)| idle > peak)
        {
            return Err(EvidenceArtifactError::InvalidField(
                "power.peak_current_microamps",
            ));
        }

        self.analyzer.fact.validate("analyzer")?;
        for (measurement, field) in [
            (self.analyzer.sample_rate_hz, "analyzer.sample_rate_hz"),
            (
                self.analyzer.capture_duration_milliseconds,
                "analyzer.capture_duration_milliseconds",
            ),
        ] {
            validate_optional_positive_measurement(
                self.analyzer.fact.was_run(),
                measurement,
                field,
            )?;
        }

        self.surface.fact.validate("surface")?;
        validate_optional_positive_measurement(
            self.surface.fact.was_run(),
            self.surface.thickness_micrometers,
            "surface.thickness_micrometers",
        )?;
        self.posture.fact.validate("posture")?;
        self.truth_device.fact.validate("truth_device")?;
        self.alignment.fact.validate("alignment")?;
        validate_optional_measurement(
            self.alignment.fact.was_run(),
            self.alignment.maximum_error_millidegrees,
            "alignment.maximum_error_millidegrees",
        )?;
        Ok(())
    }

    /// Whether every physical check required by an operational manifest passed.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.first_nonpassing_check().is_none()
    }

    fn first_nonpassing_check(&self) -> Option<&'static str> {
        self.hands
            .iter()
            .find_map(|hand| status_not_passed(&hand.mass_measurement, "hands.mass_measurement"))
            .or_else(|| status_not_passed(&self.continuity.fact, "continuity"))
            .or_else(|| status_not_passed(&self.photos.fact, "photos"))
            .or_else(|| status_not_passed(&self.power.fact, "power"))
            .or_else(|| status_not_passed(&self.analyzer.fact, "analyzer"))
            .or_else(|| status_not_passed(&self.surface.fact, "surface"))
            .or_else(|| status_not_passed(&self.posture.fact, "posture"))
            .or_else(|| status_not_passed(&self.truth_device.fact, "truth_device"))
            .or_else(|| status_not_passed(&self.alignment.fact, "alignment"))
    }

    /// Serialize the validated record to its compact canonical identity.
    pub fn canonical_json(&self) -> Result<Vec<u8>, EvidenceArtifactError> {
        self.validate()?;
        self.canonical_json_unchecked()
    }

    fn canonical_json_unchecked(&self) -> Result<Vec<u8>, EvidenceArtifactError> {
        serde_json::to_vec(self).map_err(|error| EvidenceArtifactError::Serialization {
            artifact: "rig evidence",
            message: error.to_string(),
        })
    }

    /// Hash the canonical rig evidence identity with SHA-256.
    pub fn sha256(&self) -> Result<[u8; 32], EvidenceArtifactError> {
        self.validate()?;
        self.sha256_unchecked()
    }

    fn sha256_unchecked(&self) -> Result<[u8; 32], EvidenceArtifactError> {
        Ok(sha256(&self.canonical_json_unchecked()?))
    }
}

fn validate_rig_sensor_identities<'a>(
    hand_index: usize,
    hand: &'a RigHandEvidence,
    mount_ids: &mut BTreeSet<&'a str>,
    wiring_ids: &mut BTreeSet<&'a str>,
) -> Result<(), EvidenceArtifactError> {
    let mut mux_channels = BTreeSet::new();
    let mut interrupt_pins = BTreeSet::new();
    for (position, sensor) in hand.sensors.iter().enumerate() {
        for (value, field) in [
            (&sensor.mount_id, "hands.sensors.mount_id"),
            (&sensor.wiring_id, "hands.sensors.wiring_id"),
            (&sensor.interrupt_pin, "hands.sensors.interrupt_pin"),
        ] {
            require_evidence_identity(value, field)?;
        }
        if sensor.sensor_index != position as u8 || sensor.role != CANONICAL_SENSOR_ROLES[position]
        {
            return Err(EvidenceArtifactError::InvalidSensorOrder {
                artifact: "rig evidence",
                hand_index,
                sensor_position: position,
            });
        }
        if !mux_channels.insert(sensor.mux_channel) {
            return Err(EvidenceArtifactError::DuplicateIdentity(
                "hands.sensors.mux_channel",
            ));
        }
        if !interrupt_pins.insert(sensor.interrupt_pin.as_str()) {
            return Err(EvidenceArtifactError::DuplicateIdentity(
                "hands.sensors.interrupt_pin",
            ));
        }
        if !mount_ids.insert(sensor.mount_id.as_str()) {
            return Err(EvidenceArtifactError::DuplicateIdentity(
                "hands.sensors.mount_id",
            ));
        }
        if !wiring_ids.insert(sensor.wiring_id.as_str()) {
            return Err(EvidenceArtifactError::DuplicateIdentity(
                "hands.sensors.wiring_id",
            ));
        }
    }
    Ok(())
}

impl AxisCharacterizationReport {
    /// Parse and validate one bounded six-face characterization report.
    pub fn from_json(source: &[u8]) -> Result<Self, EvidenceArtifactError> {
        if source.len() > MAX_AXIS_CHARACTERIZATION_BYTES {
            return Err(EvidenceArtifactError::TooLarge {
                artifact: "axis characterization",
                actual: source.len(),
                maximum: MAX_AXIS_CHARACTERIZATION_BYTES,
            });
        }
        let report = serde_json::from_slice::<Self>(source).map_err(|error| {
            EvidenceArtifactError::Json {
                artifact: "axis characterization",
                message: error.to_string(),
            }
        })?;
        report.validate()?;
        Ok(report)
    }

    /// Recompute every residual and pass decision from the six raw means.
    pub fn validate(&self) -> Result<(), EvidenceArtifactError> {
        if self.schema != AXIS_CHARACTERIZATION_SCHEMA {
            return Err(EvidenceArtifactError::InvalidSchema(
                "axis characterization",
            ));
        }
        for (value, field) in [
            (&self.report_id, "report_id"),
            (&self.completed_at_utc, "completed_at_utc"),
            (&self.characterizer_build, "characterizer_build"),
            (&self.method_id, "method_id"),
        ] {
            require_evidence_identity(value, field)?;
        }
        for (hash, field) in [
            (&self.candidate_manifest_sha256, "candidate_manifest_sha256"),
            (&self.source_session_sha256, "source_session_sha256"),
            (&self.rig_evidence_sha256, "rig_evidence_sha256"),
        ] {
            require_evidence_sha256(hash, field)?;
        }
        if self.coordinate_convention != CANONICAL_AXIS_CONVENTION {
            return Err(EvidenceArtifactError::InvalidCoordinateConvention);
        }
        if self.method_id != AXIS_CHARACTERIZATION_METHOD_ID {
            return Err(EvidenceArtifactError::InvalidField("method_id"));
        }
        if self.criteria != AXIS_CHARACTERIZATION_CRITERIA {
            return Err(EvidenceArtifactError::InvalidField("criteria"));
        }
        validate_hand_count_and_order(
            self.hands.iter().map(|hand| hand.hand),
            "axis characterization",
        )?;

        let mut all_passed = true;
        for (hand_index, hand) in self.hands.iter().enumerate() {
            let expected_positive_y = match hand.hand {
                ManifestHand::Left => PositiveYDirection::Ulnar,
                ManifestHand::Right => PositiveYDirection::Radial,
            };
            if hand.positive_y_direction != expected_positive_y {
                return Err(EvidenceArtifactError::PositiveYDirectionMismatch { hand: hand.hand });
            }
            for (sensor_position, sensor) in hand.sensors.iter().enumerate() {
                if sensor.sensor_index != sensor_position as u8
                    || sensor.role != CANONICAL_SENSOR_ROLES[sensor_position]
                {
                    return Err(EvidenceArtifactError::InvalidSensorOrder {
                        artifact: "axis characterization",
                        hand_index,
                        sensor_position,
                    });
                }
                if !is_proper_axis_map(sensor.axis_map) {
                    return Err(EvidenceArtifactError::InvalidAxisMap {
                        hand_index,
                        sensor_index: sensor.sensor_index,
                    });
                }
                for (position, face_mean) in sensor.raw_face_means.iter().enumerate() {
                    if face_mean.face != AXIS_FACE_ORDER[position] {
                        return Err(EvidenceArtifactError::InvalidFaceOrder {
                            hand_index,
                            sensor_index: sensor.sensor_index,
                            face_position: position,
                        });
                    }
                    if face_mean
                        .mean_acceleration_lsb
                        .iter()
                        .any(|value| !(i32::from(i16::MIN)..=i32::from(i16::MAX)).contains(value))
                    {
                        return Err(EvidenceArtifactError::InvalidField(
                            "hands.sensors.raw_face_means.mean_acceleration_lsb",
                        ));
                    }
                    if face_mean.sample_count != self.criteria.minimum_samples_per_face {
                        return Err(EvidenceArtifactError::InvalidField(
                            "hands.sensors.raw_face_means.sample_count",
                        ));
                    }
                }
                let observed = maximum_axis_residual(
                    sensor.axis_map,
                    &sensor.raw_face_means,
                    self.criteria.expected_gravity_lsb,
                );
                if observed != sensor.observed_maximum_component_residual_lsb {
                    return Err(EvidenceArtifactError::ResidualMismatch {
                        hand_index,
                        sensor_index: sensor.sensor_index,
                        declared: sensor.observed_maximum_component_residual_lsb,
                        computed: observed,
                    });
                }
                let computed_pass = observed <= self.criteria.maximum_component_residual_lsb;
                if sensor.passed != computed_pass {
                    return Err(EvidenceArtifactError::PassMismatch {
                        hand_index,
                        sensor_index: sensor.sensor_index,
                    });
                }
                all_passed &= sensor.passed;
            }
        }
        if self.passed != all_passed {
            return Err(EvidenceArtifactError::ReportPassMismatch);
        }
        Ok(())
    }

    /// Serialize the validated report to its compact canonical identity.
    pub fn canonical_json(&self) -> Result<Vec<u8>, EvidenceArtifactError> {
        self.validate()?;
        self.canonical_json_unchecked()
    }

    fn canonical_json_unchecked(&self) -> Result<Vec<u8>, EvidenceArtifactError> {
        serde_json::to_vec(self).map_err(|error| EvidenceArtifactError::Serialization {
            artifact: "axis characterization",
            message: error.to_string(),
        })
    }

    /// Hash the canonical axis-report identity with SHA-256.
    pub fn sha256(&self) -> Result<[u8; 32], EvidenceArtifactError> {
        self.validate()?;
        self.sha256_unchecked()
    }

    fn sha256_unchecked(&self) -> Result<[u8; 32], EvidenceArtifactError> {
        Ok(sha256(&self.canonical_json_unchecked()?))
    }
}

pub(crate) fn maximum_axis_residual(
    axis_map: [i8; 9],
    face_means: &[AxisFaceMean; 6],
    expected_gravity_lsb: u32,
) -> u32 {
    let gravity = i64::from(expected_gravity_lsb);
    face_means
        .iter()
        .flat_map(|face_mean| {
            let rotated = rotate_mean(axis_map, face_mean.mean_acceleration_lsb);
            let expected = expected_face_vector(face_mean.face, gravity);
            rotated
                .into_iter()
                .zip(expected)
                .map(|(actual, expected)| actual.abs_diff(expected))
        })
        .max()
        .and_then(|residual| u32::try_from(residual).ok())
        .expect("validated i16 means and gravity produce a u32 residual")
}

fn rotate_mean(axis_map: [i8; 9], vector: [i32; 3]) -> [i64; 3] {
    let mut rotated = [0_i64; 3];
    for (row, output) in rotated.iter_mut().enumerate() {
        *output = (0..3)
            .map(|column| i64::from(axis_map[row * 3 + column]) * i64::from(vector[column]))
            .sum();
    }
    rotated
}

const fn expected_face_vector(face: AxisFace, gravity: i64) -> [i64; 3] {
    match face {
        AxisFace::PositiveX => [gravity, 0, 0],
        AxisFace::NegativeX => [-gravity, 0, 0],
        AxisFace::PositiveY => [0, gravity, 0],
        AxisFace::NegativeY => [0, -gravity, 0],
        AxisFace::PositiveZ => [0, 0, gravity],
        AxisFace::NegativeZ => [0, 0, -gravity],
    }
}

/// Canonical pre-characterization projection. It deliberately excludes boot
/// IDs, selected axis maps, host/study metadata, and downstream calibration
/// artifacts. Those values either do not exist yet or do not define the
/// physical sensor coordinate system.
#[derive(Serialize)]
struct AxisCharacterizationCandidate<'a> {
    schema: &'static str,
    coordinate_convention: &'static str,
    layout_id: &'a str,
    protocol_version: u8,
    hands: Vec<AxisCharacterizationCandidateHand<'a>>,
    controller_board: &'a str,
    firmware_build: &'a str,
    sensor_model: &'a str,
    sensors_per_hand: u8,
    sensor_odr_hz: u16,
    accelerometer_range_g: u16,
    gyroscope_range_dps: u16,
    filter_profile: &'a str,
    i2c_bus_hz: u32,
    nominal_period_us: u32,
    rig_evidence_sha256: &'a str,
}

#[derive(Serialize)]
struct AxisCharacterizationCandidateHand<'a> {
    hand: ManifestHand,
    controller_id: &'a str,
    sensors: Vec<AxisCharacterizationCandidateSensor<'a>>,
}

#[derive(Serialize)]
struct AxisCharacterizationCandidateSensor<'a> {
    sensor_index: u8,
    mux_channel: u8,
    role: SensorRole,
    mount_id: &'a str,
    wiring_id: &'a str,
    interrupt_pin: &'a str,
}

/// Non-authorizing input to physical axis commissioning. It cannot be locked
/// into an acquisition manifest until the separately derived report is bound.
pub struct AxisCommissioningCandidate {
    pub(crate) pending: AcquisitionManifest,
    hash: [u8; 32],
}

impl AxisCommissioningCandidate {
    pub fn from_json(source: &[u8], rig_source: &[u8]) -> Result<Self, ManifestError> {
        if source.len() > MAX_MANIFEST_BYTES {
            return Err(ManifestError::TooLarge {
                actual: source.len(),
            });
        }
        let pending: AcquisitionManifest = serde_json::from_slice(source)
            .map_err(|error| ManifestError::Json(error.to_string()))?;
        pending.validate_template()?;
        let rig =
            RigEvidenceRecord::from_json(rig_source).map_err(ManifestError::EvidenceArtifact)?;
        if parse_sha256(&pending.rig_evidence_sha256)
            != Some(
                rig.sha256_unchecked()
                    .map_err(ManifestError::EvidenceArtifact)?,
            )
        {
            return Err(ManifestError::RigEvidenceHashMismatch);
        }
        validate_rig_manifest_match(&pending, &rig)?;
        if let Some(field) = rig.first_nonpassing_check() {
            return Err(ManifestError::RigEvidenceNotPassed(field));
        }
        let hash = pending.axis_characterization_candidate_sha256_unchecked()?;
        Ok(Self { pending, hash })
    }

    #[must_use]
    pub const fn sha256(&self) -> [u8; 32] {
        self.hash
    }

    #[must_use]
    pub fn expected_controllers(&self) -> Vec<(Hand, [u8; 32])> {
        self.pending
            .hands
            .iter()
            .map(|hand| (Hand::from(hand.hand), sha256(hand.controller_id.as_bytes())))
            .collect()
    }

    /// Only derived matrices and the report identity change. Boot IDs remain
    /// pending, and ordinary evidence-bound admission remains mandatory.
    pub fn bound_template_json(
        &self,
        report: &AxisCharacterizationReport,
    ) -> Result<Vec<u8>, ManifestError> {
        report.validate().map_err(ManifestError::EvidenceArtifact)?;
        if !report.passed {
            return Err(ManifestError::AxisCharacterizationNotPassed);
        }
        if parse_sha256(&report.candidate_manifest_sha256) != Some(self.hash) {
            return Err(ManifestError::AxisCandidateManifestHashMismatch);
        }
        if report.rig_evidence_sha256 != self.pending.rig_evidence_sha256 {
            return Err(ManifestError::AxisRigEvidenceHashMismatch);
        }
        let mut pending = self.pending.clone();
        if report.hands.len() != pending.hands.len() {
            return Err(ManifestError::EvidenceBindingMismatch("hands"));
        }
        for (hand, characterized) in pending.hands.iter_mut().zip(&report.hands) {
            if hand.hand != characterized.hand {
                return Err(ManifestError::EvidenceBindingMismatch("hands.hand"));
            }
            for (sensor, derived) in hand.sensors.iter_mut().zip(&characterized.sensors) {
                sensor.axis_map = derived.axis_map;
            }
        }
        pending.axis_characterization_sha256 =
            format_sha256(&report.sha256().map_err(ManifestError::EvidenceArtifact)?);
        serde_json::to_vec(&pending).map_err(|error| ManifestError::Json(error.to_string()))
    }
}

impl AcquisitionManifestTemplate {
    /// Parse a bounded template and bind both required evidence artifacts.
    ///
    /// The template may contain only all-zero pending boot IDs. Axis maps and
    /// physical identities must already match the passed characterization and
    /// rig records before a wire identity can lock it.
    pub fn from_json_with_evidence(
        source: &[u8],
        rig_evidence_source: &[u8],
        axis_characterization_source: &[u8],
    ) -> Result<Self, ManifestError> {
        if source.len() > MAX_MANIFEST_BYTES {
            return Err(ManifestError::TooLarge {
                actual: source.len(),
            });
        }
        let pending = serde_json::from_slice::<AcquisitionManifest>(source)
            .map_err(|error| ManifestError::Json(error.to_string()))?;
        pending.validate_template()?;
        let rig_evidence = RigEvidenceRecord::from_json(rig_evidence_source)
            .map_err(ManifestError::EvidenceArtifact)?;
        let axis_characterization =
            AxisCharacterizationReport::from_json(axis_characterization_source)
                .map_err(ManifestError::EvidenceArtifact)?;
        pending.validate_prevalidated_evidence_bindings(&rig_evidence, &axis_characterization)?;
        Ok(Self { pending })
    }

    /// Expected hand and controller identity before any boot is trusted.
    #[must_use]
    pub fn expected_controllers(&self) -> Vec<(Hand, [u8; 32])> {
        self.pending
            .hands
            .iter()
            .map(|hand| (Hand::from(hand.hand), sha256(hand.controller_id.as_bytes())))
            .collect()
    }

    /// Replace pending boot placeholders with identities observed on the wire.
    pub fn lock(
        mut self,
        observed: &[FrameIdentity],
    ) -> Result<AcquisitionManifest, ManifestError> {
        self.pending.validate_template()?;
        if observed.len() != self.pending.hands.len() {
            return Err(ManifestError::ObservedIdentityCountMismatch {
                expected: self.pending.hands.len(),
                actual: observed.len(),
            });
        }
        for (hand_index, (hand, identity)) in
            self.pending.hands.iter_mut().zip(observed).enumerate()
        {
            let expected_hand = Hand::from(hand.hand);
            if identity.hand != expected_hand {
                return Err(ManifestError::ObservedHandMismatch {
                    hand_index,
                    actual: identity.hand,
                });
            }
            if identity.controller_id_sha256 != sha256(hand.controller_id.as_bytes()) {
                return Err(ManifestError::ObservedControllerIdentityMismatch(
                    expected_hand,
                ));
            }
            if identity.boot_id == [0; 16] {
                return Err(ManifestError::InvalidBootId { hand_index });
            }
            hand.boot_id = format_id128(&identity.boot_id);
        }
        self.pending.schema = ACQUISITION_MANIFEST_SCHEMA.to_owned();
        // Boot IDs are deliberately absent from the axis-candidate projection.
        // All other evidence-bound fields are unchanged from construction.
        self.pending.validate()?;
        Ok(self.pending)
    }
}

impl AcquisitionManifest {
    /// Parse an operational manifest and bind both exact evidence artifacts.
    ///
    /// # Errors
    ///
    /// Returns a size, JSON, semantic, evidence, or binding error. Unknown and
    /// duplicate struct fields are rejected by the typed serde visitors.
    pub fn from_json_with_evidence(
        source: &[u8],
        rig_evidence_source: &[u8],
        axis_characterization_source: &[u8],
    ) -> Result<Self, ManifestError> {
        let manifest = Self::from_json(source)?;
        let rig_evidence = RigEvidenceRecord::from_json(rig_evidence_source)
            .map_err(ManifestError::EvidenceArtifact)?;
        let axis_characterization =
            AxisCharacterizationReport::from_json(axis_characterization_source)
                .map_err(ManifestError::EvidenceArtifact)?;
        manifest.validate_prevalidated_evidence_bindings(&rig_evidence, &axis_characterization)?;
        Ok(manifest)
    }

    /// Parse only manifest structure without operational evidence authority.
    ///
    /// Characterization tooling and deterministic fixtures use this structural
    /// parser. Calibration and live loaders must use `from_json_with_evidence`.
    pub fn from_json(source: &[u8]) -> Result<Self, ManifestError> {
        if source.len() > MAX_MANIFEST_BYTES {
            return Err(ManifestError::TooLarge {
                actual: source.len(),
            });
        }
        let manifest = serde_json::from_slice::<Self>(source)
            .map_err(|error| ManifestError::Json(error.to_string()))?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Compute the non-authorizing physical candidate identity from manifest JSON.
    ///
    /// This is the first phase of the axis workflow. It accepts either the
    /// acquisition-template or locked-manifest schema, validates its structure,
    /// and returns only the canonical candidate hash. It does not return an
    /// acquisition manifest and cannot authorize calibration or live use.
    pub fn axis_characterization_candidate_sha256_from_json(
        source: &[u8],
    ) -> Result<[u8; 32], ManifestError> {
        if source.len() > MAX_MANIFEST_BYTES {
            return Err(ManifestError::TooLarge {
                actual: source.len(),
            });
        }
        let manifest = serde_json::from_slice::<Self>(source)
            .map_err(|error| ManifestError::Json(error.to_string()))?;
        manifest.axis_characterization_candidate_sha256()
    }

    /// Check every current-schema invariant before hashing or capture.
    ///
    /// # Errors
    ///
    /// Returns the first unsupported or ambiguous setting.
    pub fn validate(&self) -> Result<(), ManifestError> {
        self.validate_common(ACQUISITION_MANIFEST_SCHEMA, false)
    }

    fn validate_template(&self) -> Result<(), ManifestError> {
        self.validate_common(ACQUISITION_TEMPLATE_SCHEMA, true)
    }

    fn validate_common(
        &self,
        expected_schema: &str,
        pending_boots: bool,
    ) -> Result<(), ManifestError> {
        if self.schema != expected_schema {
            if pending_boots {
                return Err(ManifestError::InvalidTemplateSchema);
            }
            return Err(ManifestError::InvalidSchema);
        }
        if self.layout_id != CORE28_LAYOUT_ID {
            return Err(ManifestError::InvalidLayout);
        }
        if self.protocol_version != PROTOCOL_VERSION {
            return Err(ManifestError::UnsupportedProtocol(self.protocol_version));
        }
        if self.hands.is_empty() || self.hands.len() > 2 {
            return Err(ManifestError::InvalidHandCount(self.hands.len()));
        }
        if self.hands.len() == 2
            && (self.hands[0].hand != ManifestHand::Left
                || self.hands[1].hand != ManifestHand::Right)
        {
            if self.hands[0].hand == self.hands[1].hand {
                return Err(ManifestError::DuplicateHand);
            }
            return Err(ManifestError::InvalidHandOrder);
        }

        let mut hand_names = BTreeSet::new();
        let mut controller_ids = BTreeSet::new();
        let mut mount_ids = BTreeSet::new();
        let mut wiring_ids = BTreeSet::new();
        for (index, hand) in self.hands.iter().enumerate() {
            if !hand_names.insert(hand.hand) {
                return Err(ManifestError::DuplicateHand);
            }
            require_identity(&hand.controller_id, "hands.controller_id")?;
            let Some(boot_id) = hand.parsed_boot_id() else {
                return Err(ManifestError::InvalidBootId { hand_index: index });
            };
            if pending_boots && boot_id != [0; 16] {
                return Err(ManifestError::TemplateBootNotPending { hand_index: index });
            }
            if !pending_boots && boot_id == [0; 16] {
                return Err(ManifestError::InvalidBootId { hand_index: index });
            }
            if !controller_ids.insert(hand.controller_id.as_str()) {
                return Err(ManifestError::DuplicateIdentity("hands.controller_id"));
            }

            let dorsal_count = hand
                .sensors
                .iter()
                .filter(|sensor| sensor.role == SensorRole::Dorsal)
                .count();
            if dorsal_count != 1 {
                return Err(ManifestError::InvalidDorsalCount {
                    hand_index: index,
                    actual: dorsal_count,
                });
            }

            let mut sensor_indices = BTreeSet::new();
            let mut sensor_roles = BTreeSet::new();
            let mut mux_channels = BTreeSet::new();
            let mut interrupt_pins = BTreeSet::new();
            for (sensor_position, sensor) in hand.sensors.iter().enumerate() {
                require_identity(&sensor.mount_id, "hands.sensors.mount_id")?;
                require_identity(&sensor.wiring_id, "hands.sensors.wiring_id")?;
                require_identity(&sensor.interrupt_pin, "hands.sensors.interrupt_pin")?;
                if !sensor_indices.insert(sensor.sensor_index) {
                    return Err(ManifestError::DuplicateSensorIndex {
                        hand_index: index,
                        sensor_index: sensor.sensor_index,
                    });
                }
                if !sensor_roles.insert(sensor.role) {
                    return Err(ManifestError::DuplicateSensorRole {
                        hand_index: index,
                        role: sensor.role,
                    });
                }
                if !mux_channels.insert(sensor.mux_channel) {
                    return Err(ManifestError::DuplicateMuxChannel {
                        hand_index: index,
                        mux_channel: sensor.mux_channel,
                    });
                }
                if !interrupt_pins.insert(sensor.interrupt_pin.as_str()) {
                    return Err(ManifestError::DuplicateIdentity(
                        "hands.sensors.interrupt_pin",
                    ));
                }
                if !mount_ids.insert(sensor.mount_id.as_str()) {
                    return Err(ManifestError::DuplicateIdentity("hands.sensors.mount_id"));
                }
                if !wiring_ids.insert(sensor.wiring_id.as_str()) {
                    return Err(ManifestError::DuplicateIdentity("hands.sensors.wiring_id"));
                }
                if !is_proper_axis_map(sensor.axis_map) {
                    return Err(ManifestError::InvalidAxisMap {
                        hand_index: index,
                        sensor_index: sensor.sensor_index,
                    });
                }
                if sensor.sensor_index != sensor_position as u8
                    || sensor.role != CANONICAL_ACQUISITION_PROFILE.sensor_order[sensor_position]
                {
                    return Err(ManifestError::InvalidSensorOrder {
                        hand_index: index,
                        sensor_position,
                    });
                }
            }
        }

        for (value, field) in [
            (&self.controller_board, "controller_board"),
            (&self.firmware_build, "firmware_build"),
            (&self.host_build, "host_build"),
            (&self.sensor_model, "sensor_model"),
            (&self.filter_profile, "filter_profile"),
            (&self.study_id, "study_id"),
            (&self.split_id, "split_id"),
            (&self.participant_alias, "participant_alias"),
            (&self.consent_record, "consent_record"),
            (&self.retention_policy, "retention_policy"),
        ] {
            require_identity(value, field)?;
        }

        for (matches_profile, field) in [
            (
                usize::from(self.sensors_per_hand)
                    == CANONICAL_ACQUISITION_PROFILE.sensor_order.len(),
                "sensors_per_hand",
            ),
            (
                self.sensor_odr_hz == CANONICAL_ACQUISITION_PROFILE.sensor_odr_hz,
                "sensor_odr_hz",
            ),
            (
                self.accelerometer_range_g == CANONICAL_ACQUISITION_PROFILE.accelerometer_range_g,
                "accelerometer_range_g",
            ),
            (
                self.gyroscope_range_dps == CANONICAL_ACQUISITION_PROFILE.gyroscope_range_dps,
                "gyroscope_range_dps",
            ),
            (
                self.i2c_bus_hz == CANONICAL_ACQUISITION_PROFILE.i2c_bus_hz,
                "i2c_bus_hz",
            ),
            (
                self.nominal_period_us == CANONICAL_ACQUISITION_PROFILE.nominal_period_us,
                "nominal_period_us",
            ),
        ] {
            if !matches_profile {
                return Err(ManifestError::InvalidSetting(field));
            }
        }
        require_sha256(&self.prompt_corpus_sha256, "prompt_corpus_sha256")?;
        require_sha256(&self.rig_evidence_sha256, "rig_evidence_sha256")?;
        require_sha256(
            &self.axis_characterization_sha256,
            "axis_characterization_sha256",
        )?;
        if let Some(hash) = &self.calibration_artifact_sha256 {
            require_sha256(hash, "calibration_artifact_sha256")?;
        }
        if let Some(hash) = &self.calibration_validation_sha256 {
            require_sha256(hash, "calibration_validation_sha256")?;
        }
        Ok(())
    }

    /// Validate exact rig and axis evidence bindings for operational use.
    ///
    /// Structural validation alone is not an authorization boundary. Runtime
    /// loaders must use `from_json_with_evidence`, which calls this method with
    /// both immutable artifacts before returning a manifest.
    pub fn validate_with_evidence(
        &self,
        rig_evidence: &RigEvidenceRecord,
        axis_characterization: &AxisCharacterizationReport,
    ) -> Result<(), ManifestError> {
        self.validate()?;
        self.validate_evidence_bindings(rig_evidence, axis_characterization)
    }

    fn validate_evidence_bindings(
        &self,
        rig_evidence: &RigEvidenceRecord,
        axis_characterization: &AxisCharacterizationReport,
    ) -> Result<(), ManifestError> {
        rig_evidence
            .validate()
            .map_err(ManifestError::EvidenceArtifact)?;
        axis_characterization
            .validate()
            .map_err(ManifestError::EvidenceArtifact)?;
        self.validate_prevalidated_evidence_bindings(rig_evidence, axis_characterization)
    }

    fn validate_prevalidated_evidence_bindings(
        &self,
        rig_evidence: &RigEvidenceRecord,
        axis_characterization: &AxisCharacterizationReport,
    ) -> Result<(), ManifestError> {
        let rig_hash = rig_evidence
            .sha256_unchecked()
            .map_err(ManifestError::EvidenceArtifact)?;
        if parse_sha256(&self.rig_evidence_sha256) != Some(rig_hash) {
            return Err(ManifestError::RigEvidenceHashMismatch);
        }
        let axis_hash = axis_characterization
            .sha256_unchecked()
            .map_err(ManifestError::EvidenceArtifact)?;
        if parse_sha256(&self.axis_characterization_sha256) != Some(axis_hash) {
            return Err(ManifestError::AxisCharacterizationHashMismatch);
        }
        if let Some(field) = rig_evidence.first_nonpassing_check() {
            return Err(ManifestError::RigEvidenceNotPassed(field));
        }
        if !axis_characterization.passed {
            return Err(ManifestError::AxisCharacterizationNotPassed);
        }
        if parse_sha256(&axis_characterization.rig_evidence_sha256) != Some(rig_hash) {
            return Err(ManifestError::AxisRigEvidenceHashMismatch);
        }
        if parse_sha256(&axis_characterization.candidate_manifest_sha256)
            != Some(self.axis_characterization_candidate_sha256_unchecked()?)
        {
            return Err(ManifestError::AxisCandidateManifestHashMismatch);
        }
        validate_rig_manifest_match(self, rig_evidence)?;
        validate_axis_manifest_match(self, axis_characterization)?;
        Ok(())
    }

    /// Canonical physical/acquisition projection to be bound by an axis report.
    pub fn axis_characterization_candidate_json(&self) -> Result<Vec<u8>, ManifestError> {
        match self.schema.as_str() {
            ACQUISITION_MANIFEST_SCHEMA => self.validate()?,
            ACQUISITION_TEMPLATE_SCHEMA => self.validate_template()?,
            _ => return Err(ManifestError::InvalidSchema),
        }
        self.axis_characterization_candidate_json_unchecked()
    }

    fn axis_characterization_candidate_json_unchecked(&self) -> Result<Vec<u8>, ManifestError> {
        let hands = self
            .hands
            .iter()
            .map(|hand| AxisCharacterizationCandidateHand {
                hand: hand.hand,
                controller_id: &hand.controller_id,
                sensors: hand
                    .sensors
                    .iter()
                    .map(|sensor| AxisCharacterizationCandidateSensor {
                        sensor_index: sensor.sensor_index,
                        mux_channel: sensor.mux_channel,
                        role: sensor.role,
                        mount_id: &sensor.mount_id,
                        wiring_id: &sensor.wiring_id,
                        interrupt_pin: &sensor.interrupt_pin,
                    })
                    .collect(),
            })
            .collect();
        let candidate = AxisCharacterizationCandidate {
            schema: AXIS_CHARACTERIZATION_CANDIDATE_SCHEMA,
            coordinate_convention: CANONICAL_AXIS_CONVENTION,
            layout_id: &self.layout_id,
            protocol_version: self.protocol_version,
            hands,
            controller_board: &self.controller_board,
            firmware_build: &self.firmware_build,
            sensor_model: &self.sensor_model,
            sensors_per_hand: self.sensors_per_hand,
            sensor_odr_hz: self.sensor_odr_hz,
            accelerometer_range_g: self.accelerometer_range_g,
            gyroscope_range_dps: self.gyroscope_range_dps,
            filter_profile: &self.filter_profile,
            i2c_bus_hz: self.i2c_bus_hz,
            nominal_period_us: self.nominal_period_us,
            rig_evidence_sha256: &self.rig_evidence_sha256,
        };
        serde_json::to_vec(&candidate)
            .map_err(|error| ManifestError::Serialization(error.to_string()))
    }

    /// Hash the canonical pre-characterization physical candidate projection.
    pub fn axis_characterization_candidate_sha256(&self) -> Result<[u8; 32], ManifestError> {
        Ok(sha256(&self.axis_characterization_candidate_json()?))
    }

    fn axis_characterization_candidate_sha256_unchecked(&self) -> Result<[u8; 32], ManifestError> {
        Ok(sha256(
            &self.axis_characterization_candidate_json_unchecked()?,
        ))
    }

    /// Serialize the validated typed value to its one compact JSON identity.
    ///
    /// # Errors
    ///
    /// Returns a semantic error or an unexpected serializer error.
    pub fn canonical_json(&self) -> Result<Vec<u8>, ManifestError> {
        self.validate()?;
        serde_json::to_vec(self).map_err(|error| ManifestError::Serialization(error.to_string()))
    }

    /// Hash the canonical JSON with SHA-256.
    ///
    /// # Errors
    ///
    /// Returns any validation or serialization error.
    pub fn sha256(&self) -> Result<[u8; 32], ManifestError> {
        let bytes = self.canonical_json()?;
        Ok(sha256(&bytes))
    }

    /// Bind observed decoded-frame counts to the declared controller hands.
    ///
    /// A missing declared hand and an undeclared observed hand both invalidate
    /// provenance, even when the session container itself is byte-correct.
    pub fn validate_observed_topology(
        &self,
        left_frames: u64,
        right_frames: u64,
    ) -> Result<(), ObservedTopologyError> {
        self.validate_no_undeclared_hands(left_frames, right_frames)?;
        for (hand, frames) in [
            (ManifestHand::Left, left_frames),
            (ManifestHand::Right, right_frames),
        ] {
            if frames == 0 && self.hands.iter().any(|candidate| candidate.hand == hand) {
                return Err(ObservedTopologyError::MissingDeclaredHand(hand));
            }
        }
        Ok(())
    }

    /// Reject decoded frames from a controller hand absent from the manifest.
    /// Partial diagnostic captures may still omit a declared hand; readiness
    /// and fitting call `validate_observed_topology` for the stronger gate.
    pub fn validate_no_undeclared_hands(
        &self,
        left_frames: u64,
        right_frames: u64,
    ) -> Result<(), ObservedTopologyError> {
        self.validate().map_err(ObservedTopologyError::Manifest)?;
        for (hand, frames) in [
            (ManifestHand::Left, left_frames),
            (ManifestHand::Right, right_frames),
        ] {
            let declared = self.hands.iter().any(|candidate| candidate.hand == hand);
            if !declared && frames > 0 {
                return Err(ObservedTopologyError::UndeclaredObservedHand(hand));
            }
        }
        Ok(())
    }

    /// Return one validated wire identity per declared controller.
    ///
    /// # Errors
    ///
    /// Returns a manifest error before exposing any derived identity.
    pub fn frame_identities(&self) -> Result<Vec<FrameIdentity>, ManifestError> {
        self.validate()?;
        Ok(self
            .hands
            .iter()
            .map(|hand| {
                hand.frame_identity()
                    .expect("validated hand has a boot identity")
            })
            .collect())
    }

    /// Prove that one decoded frame came from its declared controller boot.
    ///
    /// # Errors
    ///
    /// Returns the exact missing or mismatched identity field.
    pub fn validate_frame_identity(&self, frame: &SensorFrame) -> Result<(), FrameIdentityError> {
        self.validate().map_err(FrameIdentityError::Manifest)?;
        let expected = self
            .hands
            .iter()
            .find_map(|hand| {
                let identity = hand
                    .frame_identity()
                    .expect("validated hand has a boot identity");
                (identity.hand == frame.hand).then_some(identity)
            })
            .ok_or(FrameIdentityError::UndeclaredHand(frame.hand))?;
        if frame.controller_id_sha256 != expected.controller_id_sha256 {
            return Err(FrameIdentityError::ControllerIdentityMismatch(frame.hand));
        }
        if frame.boot_id != expected.boot_id {
            return Err(FrameIdentityError::BootIdentityMismatch(frame.hand));
        }
        Ok(())
    }
}

fn validate_rig_manifest_match(
    manifest: &AcquisitionManifest,
    rig: &RigEvidenceRecord,
) -> Result<(), ManifestError> {
    if manifest.controller_board != rig.controller_board {
        return Err(ManifestError::EvidenceBindingMismatch("controller_board"));
    }
    if manifest.sensor_model != rig.sensor_model {
        return Err(ManifestError::EvidenceBindingMismatch("sensor_model"));
    }
    if manifest.hands.len() != rig.hands.len() {
        return Err(ManifestError::EvidenceBindingMismatch("hands"));
    }
    for (manifest_hand, rig_hand) in manifest.hands.iter().zip(&rig.hands) {
        if manifest_hand.hand != rig_hand.hand {
            return Err(ManifestError::EvidenceBindingMismatch("hands.hand"));
        }
        if manifest_hand.controller_id != rig_hand.controller_id {
            return Err(ManifestError::EvidenceBindingMismatch(
                "hands.controller_id",
            ));
        }
        for (manifest_sensor, rig_sensor) in manifest_hand.sensors.iter().zip(&rig_hand.sensors) {
            if manifest_sensor.sensor_index != rig_sensor.sensor_index {
                return Err(ManifestError::EvidenceBindingMismatch(
                    "hands.sensors.sensor_index",
                ));
            }
            if manifest_sensor.mux_channel != rig_sensor.mux_channel {
                return Err(ManifestError::EvidenceBindingMismatch(
                    "hands.sensors.mux_channel",
                ));
            }
            if manifest_sensor.role != rig_sensor.role {
                return Err(ManifestError::EvidenceBindingMismatch("hands.sensors.role"));
            }
            if manifest_sensor.mount_id != rig_sensor.mount_id {
                return Err(ManifestError::EvidenceBindingMismatch(
                    "hands.sensors.mount_id",
                ));
            }
            if manifest_sensor.wiring_id != rig_sensor.wiring_id {
                return Err(ManifestError::EvidenceBindingMismatch(
                    "hands.sensors.wiring_id",
                ));
            }
            if manifest_sensor.interrupt_pin != rig_sensor.interrupt_pin {
                return Err(ManifestError::EvidenceBindingMismatch(
                    "hands.sensors.interrupt_pin",
                ));
            }
        }
    }
    Ok(())
}

fn validate_axis_manifest_match(
    manifest: &AcquisitionManifest,
    report: &AxisCharacterizationReport,
) -> Result<(), ManifestError> {
    if manifest.hands.len() != report.hands.len() {
        return Err(ManifestError::EvidenceBindingMismatch("axis.hands"));
    }
    for (hand_index, (manifest_hand, report_hand)) in
        manifest.hands.iter().zip(&report.hands).enumerate()
    {
        if manifest_hand.hand != report_hand.hand {
            return Err(ManifestError::EvidenceBindingMismatch("axis.hands.hand"));
        }
        for (manifest_sensor, report_sensor) in
            manifest_hand.sensors.iter().zip(&report_hand.sensors)
        {
            if manifest_sensor.sensor_index != report_sensor.sensor_index
                || manifest_sensor.role != report_sensor.role
            {
                return Err(ManifestError::EvidenceBindingMismatch(
                    "axis.hands.sensors.identity",
                ));
            }
            if manifest_sensor.axis_map != report_sensor.axis_map {
                return Err(ManifestError::CharacterizedAxisMapMismatch {
                    hand_index,
                    sensor_index: manifest_sensor.sensor_index,
                });
            }
        }
    }
    Ok(())
}

/// Why a decoded frame does not match the immutable acquisition manifest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FrameIdentityError {
    Manifest(ManifestError),
    UndeclaredHand(Hand),
    ControllerIdentityMismatch(Hand),
    BootIdentityMismatch(Hand),
}

impl fmt::Display for FrameIdentityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Manifest(error) => error.fmt(formatter),
            Self::UndeclaredHand(hand) => {
                write!(
                    formatter,
                    "frame hand {hand:?} is not declared by the manifest"
                )
            }
            Self::ControllerIdentityMismatch(hand) => write!(
                formatter,
                "frame controller identity for {hand:?} does not match the manifest"
            ),
            Self::BootIdentityMismatch(hand) => write!(
                formatter,
                "frame boot identity for {hand:?} does not match the manifest"
            ),
        }
    }
}

impl Error for FrameIdentityError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Manifest(error) => Some(error),
            Self::UndeclaredHand(_)
            | Self::ControllerIdentityMismatch(_)
            | Self::BootIdentityMismatch(_) => None,
        }
    }
}

impl fmt::Display for FrameCanonicalizationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Identity(error) => error.fmt(formatter),
            Self::AxisOverflow {
                hand,
                sensor_index,
                vector,
                axis,
            } => write!(
                formatter,
                "frame {hand:?} sensor {sensor_index} {vector:?} axis {axis} overflows canonical i16 coordinates"
            ),
        }
    }
}

impl Error for FrameCanonicalizationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Identity(error) => Some(error),
            Self::AxisOverflow { .. } => None,
        }
    }
}

/// Compute SHA-256 for any immutable evidence bytes.
#[must_use]
pub fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

/// Format a SHA-256 value as lowercase hexadecimal.
#[must_use]
pub fn format_sha256(hash: &[u8; 32]) -> String {
    format_lower_hex(hash)
}

/// Parse one lowercase hexadecimal SHA-256 identity.
#[must_use]
pub fn parse_sha256(value: &str) -> Option<[u8; 32]> {
    parse_lower_hex(value)
}

/// Parse one lowercase hexadecimal 128-bit identity.
#[must_use]
pub fn parse_id128(value: &str) -> Option<[u8; 16]> {
    parse_lower_hex(value)
}

/// Format a 128-bit identity as lowercase hexadecimal.
#[must_use]
pub fn format_id128(identity: &[u8; 16]) -> String {
    format_lower_hex(identity)
}

pub(crate) fn format_lower_hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        write!(&mut output, "{byte:02x}").expect("writing to a String cannot fail");
    }
    output
}

fn parse_lower_hex<const N: usize>(value: &str) -> Option<[u8; N]> {
    if value.len() != N * 2
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return None;
    }
    let mut output = [0_u8; N];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        output[index] = (hex_nibble(pair[0])? << 4) | hex_nibble(pair[1])?;
    }
    Some(output)
}

fn require_identity(value: &str, field: &'static str) -> Result<(), ManifestError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(ManifestError::EmptyIdentity(field));
    }
    if value
        .get(..PLACEHOLDER_IDENTITY_PREFIX.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(PLACEHOLDER_IDENTITY_PREFIX))
        || PLACEHOLDER_IDENTITIES
            .iter()
            .any(|placeholder| value.eq_ignore_ascii_case(placeholder))
    {
        return Err(ManifestError::PlaceholderIdentity(field));
    }
    Ok(())
}

fn require_sha256(value: &str, field: &'static str) -> Result<(), ManifestError> {
    if parse_sha256(value).is_none_or(|hash| hash == [0; 32]) {
        return Err(ManifestError::InvalidSha256(field));
    }
    Ok(())
}

fn require_evidence_identity(
    value: &str,
    field: &'static str,
) -> Result<(), EvidenceArtifactError> {
    require_identity(value, field).map_err(|_| EvidenceArtifactError::InvalidField(field))
}

fn require_evidence_sha256(value: &str, field: &'static str) -> Result<(), EvidenceArtifactError> {
    require_sha256(value, field).map_err(|_| EvidenceArtifactError::InvalidSha256(field))
}

fn validate_hand_count_and_order(
    mut hands: impl ExactSizeIterator<Item = ManifestHand>,
    artifact: &'static str,
) -> Result<(), EvidenceArtifactError> {
    let actual = hands.len();
    if actual == 0 || actual > 2 {
        return Err(EvidenceArtifactError::InvalidHandCount { artifact, actual });
    }
    if actual == 2
        && (hands.next() != Some(ManifestHand::Left) || hands.next() != Some(ManifestHand::Right))
    {
        return Err(EvidenceArtifactError::InvalidHandOrder(artifact));
    }
    Ok(())
}

fn validate_optional_positive_measurement(
    was_run: bool,
    measurement: Option<u32>,
    field: &'static str,
) -> Result<(), EvidenceArtifactError> {
    if was_run != measurement.is_some_and(|value| value > 0) {
        return Err(EvidenceArtifactError::InconsistentField(field));
    }
    Ok(())
}

fn validate_optional_measurement(
    was_run: bool,
    measurement: Option<u32>,
    field: &'static str,
) -> Result<(), EvidenceArtifactError> {
    if was_run != measurement.is_some() {
        return Err(EvidenceArtifactError::InconsistentField(field));
    }
    Ok(())
}

fn status_not_passed(fact: &EvidenceFact, field: &'static str) -> Option<&'static str> {
    (fact.status != EvidenceStatus::Passed).then_some(field)
}

const fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

fn is_signed_permutation(matrix: [i8; 9]) -> bool {
    for row in 0..3 {
        if (0..3)
            .filter(|&column| matrix[row * 3 + column] != 0)
            .count()
            != 1
        {
            return false;
        }
    }
    for column in 0..3 {
        if (0..3).filter(|&row| matrix[row * 3 + column] != 0).count() != 1 {
            return false;
        }
    }
    matrix.into_iter().all(|value| (-1..=1).contains(&value))
}

pub(crate) fn is_proper_axis_map(matrix: [i8; 9]) -> bool {
    // Reflections reverse handedness. Axis normalization accepts only the 24
    // determinant-positive rotations representable as signed permutations.
    is_signed_permutation(matrix) && determinant(matrix) == 1
}

const fn determinant(matrix: [i8; 9]) -> i8 {
    matrix[0] * (matrix[4] * matrix[8] - matrix[5] * matrix[7])
        - matrix[1] * (matrix[3] * matrix[8] - matrix[5] * matrix[6])
        + matrix[2] * (matrix[3] * matrix[7] - matrix[4] * matrix[6])
}

/// Why a physical evidence artifact cannot become a canonical identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EvidenceArtifactError {
    TooLarge {
        artifact: &'static str,
        actual: usize,
        maximum: usize,
    },
    Json {
        artifact: &'static str,
        message: String,
    },
    Serialization {
        artifact: &'static str,
        message: String,
    },
    InvalidSchema(&'static str),
    InvalidField(&'static str),
    InconsistentField(&'static str),
    InvalidSha256(&'static str),
    DuplicateIdentity(&'static str),
    InvalidHandCount {
        artifact: &'static str,
        actual: usize,
    },
    InvalidHandOrder(&'static str),
    InvalidSensorOrder {
        artifact: &'static str,
        hand_index: usize,
        sensor_position: usize,
    },
    InvalidAxisMap {
        hand_index: usize,
        sensor_index: u8,
    },
    InvalidFaceOrder {
        hand_index: usize,
        sensor_index: u8,
        face_position: usize,
    },
    ResidualMismatch {
        hand_index: usize,
        sensor_index: u8,
        declared: u32,
        computed: u32,
    },
    PassMismatch {
        hand_index: usize,
        sensor_index: u8,
    },
    ReportPassMismatch,
    InvalidCoordinateConvention,
    PositiveYDirectionMismatch {
        hand: ManifestHand,
    },
}

impl fmt::Display for EvidenceArtifactError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge {
                artifact,
                actual,
                maximum,
            } => write!(
                formatter,
                "{artifact} is {actual} bytes; maximum is {maximum}"
            ),
            Self::Json { artifact, message } => {
                write!(formatter, "{artifact} JSON is invalid: {message}")
            }
            Self::Serialization { artifact, message } => {
                write!(formatter, "{artifact} serialization failed: {message}")
            }
            Self::InvalidSchema(artifact) => {
                write!(formatter, "{artifact} schema is unsupported")
            }
            Self::InvalidField(field) => {
                write!(formatter, "evidence field {field} is invalid")
            }
            Self::InconsistentField(field) => {
                write!(formatter, "evidence field {field} conflicts with its check status")
            }
            Self::InvalidSha256(field) => {
                write!(formatter, "evidence field {field} is not a nonzero lowercase SHA-256")
            }
            Self::DuplicateIdentity(field) => {
                write!(formatter, "evidence repeats {field}")
            }
            Self::InvalidHandCount { artifact, actual } => {
                write!(formatter, "{artifact} has {actual} hands; expected one or two")
            }
            Self::InvalidHandOrder(artifact) => {
                write!(formatter, "two-hand {artifact} must order left before right")
            }
            Self::InvalidSensorOrder {
                artifact,
                hand_index,
                sensor_position,
            } => write!(
                formatter,
                "{artifact} hand {hand_index} sensor position {sensor_position} is not canonical"
            ),
            Self::InvalidAxisMap {
                hand_index,
                sensor_index,
            } => write!(
                formatter,
                "axis characterization hand {hand_index} sensor {sensor_index} map is not a determinant-positive signed permutation"
            ),
            Self::InvalidFaceOrder {
                hand_index,
                sensor_index,
                face_position,
            } => write!(
                formatter,
                "axis characterization hand {hand_index} sensor {sensor_index} face position {face_position} is not canonical"
            ),
            Self::ResidualMismatch {
                hand_index,
                sensor_index,
                declared,
                computed,
            } => write!(
                formatter,
                "axis characterization hand {hand_index} sensor {sensor_index} declares residual {declared}; computed {computed}"
            ),
            Self::PassMismatch {
                hand_index,
                sensor_index,
            } => write!(
                formatter,
                "axis characterization hand {hand_index} sensor {sensor_index} pass result conflicts with raw means and criteria"
            ),
            Self::ReportPassMismatch => formatter.write_str(
                "axis characterization report pass result conflicts with its sensor results",
            ),
            Self::InvalidCoordinateConvention => formatter.write_str(
                "axis characterization coordinate convention is not the canonical right-handed carrier frame",
            ),
            Self::PositiveYDirectionMismatch { hand } => write!(
                formatter,
                "axis characterization {hand:?} hand declares the wrong canonical +Y direction"
            ),
        }
    }
}

impl Error for EvidenceArtifactError {}

/// Why an acquisition manifest cannot become evidence identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ManifestError {
    TooLarge {
        actual: usize,
    },
    Json(String),
    Serialization(String),
    InvalidSchema,
    InvalidTemplateSchema,
    InvalidLayout,
    UnsupportedProtocol(u8),
    InvalidHandCount(usize),
    InvalidHandOrder,
    DuplicateHand,
    DuplicateIdentity(&'static str),
    EmptyIdentity(&'static str),
    PlaceholderIdentity(&'static str),
    DuplicateSensorIndex {
        hand_index: usize,
        sensor_index: u8,
    },
    DuplicateSensorRole {
        hand_index: usize,
        role: SensorRole,
    },
    DuplicateMuxChannel {
        hand_index: usize,
        mux_channel: u8,
    },
    InvalidDorsalCount {
        hand_index: usize,
        actual: usize,
    },
    InvalidSensorOrder {
        hand_index: usize,
        sensor_position: usize,
    },
    InvalidAxisMap {
        hand_index: usize,
        sensor_index: u8,
    },
    InvalidBootId {
        hand_index: usize,
    },
    TemplateBootNotPending {
        hand_index: usize,
    },
    ObservedIdentityCountMismatch {
        expected: usize,
        actual: usize,
    },
    ObservedHandMismatch {
        hand_index: usize,
        actual: Hand,
    },
    ObservedControllerIdentityMismatch(Hand),
    InvalidSetting(&'static str),
    InvalidSha256(&'static str),
    EvidenceArtifact(EvidenceArtifactError),
    RigEvidenceHashMismatch,
    AxisCharacterizationHashMismatch,
    RigEvidenceNotPassed(&'static str),
    AxisCharacterizationNotPassed,
    AxisRigEvidenceHashMismatch,
    AxisCandidateManifestHashMismatch,
    EvidenceBindingMismatch(&'static str),
    CharacterizedAxisMapMismatch {
        hand_index: usize,
        sensor_index: u8,
    },
}

impl fmt::Display for ManifestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge { actual } => write!(
                formatter,
                "manifest is {actual} bytes; maximum is {MAX_MANIFEST_BYTES}"
            ),
            Self::Json(error) => write!(formatter, "manifest JSON is invalid: {error}"),
            Self::Serialization(error) => {
                write!(formatter, "manifest serialization failed: {error}")
            }
            Self::InvalidSchema => formatter.write_str("manifest schema is unsupported"),
            Self::InvalidTemplateSchema => {
                formatter.write_str("acquisition template schema is unsupported")
            }
            Self::InvalidLayout => formatter.write_str("manifest layout is unsupported"),
            Self::UnsupportedProtocol(version) => {
                write!(
                    formatter,
                    "manifest protocol version {version} is unsupported"
                )
            }
            Self::InvalidHandCount(count) => {
                write!(formatter, "manifest has {count} hands; expected one or two")
            }
            Self::InvalidHandOrder => {
                formatter.write_str("two-hand manifest must order left before right")
            }
            Self::DuplicateHand => formatter.write_str("manifest repeats a hand"),
            Self::DuplicateIdentity(field) => write!(formatter, "manifest repeats {field}"),
            Self::EmptyIdentity(field) => write!(formatter, "manifest field {field} is empty"),
            Self::PlaceholderIdentity(field) => {
                write!(formatter, "manifest field {field} is a placeholder")
            }
            Self::DuplicateSensorIndex {
                hand_index,
                sensor_index,
            } => {
                write!(
                    formatter,
                    "manifest hand {hand_index} repeats sensor index {sensor_index}"
                )
            }
            Self::DuplicateSensorRole { hand_index, role } => {
                write!(
                    formatter,
                    "manifest hand {hand_index} repeats sensor role {role:?}"
                )
            }
            Self::DuplicateMuxChannel {
                hand_index,
                mux_channel,
            } => write!(
                formatter,
                "manifest hand {hand_index} repeats mux channel {mux_channel}"
            ),
            Self::InvalidDorsalCount { hand_index, actual } => write!(
                formatter,
                "manifest hand {hand_index} has {actual} dorsal sensors; expected one"
            ),
            Self::InvalidSensorOrder {
                hand_index,
                sensor_position,
            } => write!(
                formatter,
                "manifest hand {hand_index} sensor position {sensor_position} is not canonical"
            ),
            Self::InvalidAxisMap {
                hand_index,
                sensor_index,
            } => write!(
                formatter,
                "manifest hand {hand_index} sensor {sensor_index} axis map is not a determinant-positive signed permutation"
            ),
            Self::InvalidBootId { hand_index } => {
                write!(
                    formatter,
                    "manifest hand {hand_index} boot ID is not lowercase 128-bit hex"
                )
            }
            Self::TemplateBootNotPending { hand_index } => write!(
                formatter,
                "acquisition template hand {hand_index} boot ID must be all zero before capture"
            ),
            Self::ObservedIdentityCountMismatch { expected, actual } => write!(
                formatter,
                "capture observed {actual} controller identities; template declares {expected}"
            ),
            Self::ObservedHandMismatch { hand_index, actual } => write!(
                formatter,
                "capture identity {hand_index} claims {actual:?}; template order differs"
            ),
            Self::ObservedControllerIdentityMismatch(hand) => write!(
                formatter,
                "observed {hand:?} controller identity does not match the template"
            ),
            Self::InvalidSetting(field) => {
                write!(formatter, "manifest setting {field} is invalid")
            }
            Self::InvalidSha256(field) => {
                write!(formatter, "manifest field {field} is not lowercase SHA-256")
            }
            Self::EvidenceArtifact(error) => error.fmt(formatter),
            Self::RigEvidenceHashMismatch => formatter.write_str(
                "manifest rig_evidence_sha256 does not bind the passed rig evidence record",
            ),
            Self::AxisCharacterizationHashMismatch => formatter.write_str(
                "manifest axis_characterization_sha256 does not bind the passed axis report",
            ),
            Self::RigEvidenceNotPassed(field) => {
                write!(formatter, "rig evidence check {field} has not passed")
            }
            Self::AxisCharacterizationNotPassed => {
                formatter.write_str("axis characterization report did not pass")
            }
            Self::AxisRigEvidenceHashMismatch => formatter
                .write_str("axis characterization report does not bind the manifest rig evidence"),
            Self::AxisCandidateManifestHashMismatch => formatter.write_str(
                "axis characterization report does not bind the manifest physical candidate",
            ),
            Self::EvidenceBindingMismatch(field) => write!(
                formatter,
                "manifest field {field} does not match its physical evidence"
            ),
            Self::CharacterizedAxisMapMismatch {
                hand_index,
                sensor_index,
            } => write!(
                formatter,
                "manifest hand {hand_index} sensor {sensor_index} axis map does not match the passed characterization"
            ),
        }
    }
}

impl Error for ManifestError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::EvidenceArtifact(error) => Some(error),
            _ => None,
        }
    }
}

/// Why decoded session hands do not match an acquisition manifest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ObservedTopologyError {
    Manifest(ManifestError),
    MissingDeclaredHand(ManifestHand),
    UndeclaredObservedHand(ManifestHand),
}

impl fmt::Display for ObservedTopologyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Manifest(error) => {
                write!(formatter, "observed topology manifest failed: {error}")
            }
            Self::MissingDeclaredHand(hand) => {
                write!(
                    formatter,
                    "session has no decoded frames for declared {hand:?} hand"
                )
            }
            Self::UndeclaredObservedHand(hand) => {
                write!(
                    formatter,
                    "session contains decoded frames for undeclared {hand:?} hand"
                )
            }
        }
    }
}

impl Error for ObservedTopologyError {}
