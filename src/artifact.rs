//! Inspectable, versioned personal calibration artifact and strict loader.

use std::{collections::BTreeSet, error::Error, fmt};

use serde::{Deserialize, Serialize};

use crate::{
    calibration::{ArtifactIdentityError, LockedArtifactIdentity},
    clock::{ClockError, ClockFit},
    decoder::{
        DISTANCE_MARGIN, DecoderArtifactError, FEATURE_COUNT, FINGER_ENERGY_RATIO, HandDecoder,
        MAX_INTER_SENSOR_SKEW_US, VARIANCE_FLOOR,
    },
    layout::{Hand, PHYSICAL_POSITION_COUNT, core28_layout_manifest_bytes},
    manifest::{
        CANONICAL_SENSOR_ROLES, CORE28_LAYOUT_ID, SensorManifest, format_sha256,
        is_proper_axis_map, parse_sha256, sha256,
    },
    pipeline::{DECISION_DELAY_FRAMES, REARM_IDLE_WINDOWS, WINDOW_FRAMES},
    prompts::PromptCorpus,
};

pub const CALIBRATION_ARTIFACT_SCHEMA: &str = "invisible-keyboard-calibration-artifact-v4";
pub const FEATURE_SCHEMA: &str = "relative-imu-peak-v1";
pub const POSITION_TRAINING_POLICY: &str = "passive-surface-only-v1";
pub const US_QWERTY_LAYOUT_ID: &str = "us-qwerty-v1";
pub const US_QWERTY_CONFIRMATION_PROMPT_ID: &str = "confirm-us-qwerty-v1";
pub const US_QWERTY_CONFIRMATION_COPY: &str = "Q W E R T Y U I O P / A S D F G H J K L / Z X C V B N M. Setup reads these physical key positions; the active OS language does not relabel them.";
pub const MAX_ARTIFACT_BYTES: usize = 1_048_576;
pub const MIN_ARTIFACT_POSITION_EXAMPLES: u16 = 3;

const SENSORS_PER_HAND: usize = CANONICAL_SENSOR_ROLES.len();

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ArtifactSourceRole {
    Training,
    ThresholdSelection,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ArtifactDomain {
    Keyboard,
    PassiveSurface,
}

/// One immutable source session and the blocks it contributes.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactSource {
    pub session_sha256: String,
    pub role: ArtifactSourceRole,
    pub domain: ArtifactDomain,
    pub block_ids: Vec<u32>,
}

impl ArtifactSource {
    #[must_use]
    pub fn new(
        session_sha256: [u8; 32],
        role: ArtifactSourceRole,
        domain: ArtifactDomain,
        block_ids: Vec<u32>,
    ) -> Self {
        Self {
            session_sha256: format_sha256(&session_sha256),
            role,
            domain,
            block_ids,
        }
    }
}

/// Browser receipt proving which layout prompt the user accepted.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct KeyboardLayoutConfirmationReceipt {
    pub layout_id: String,
    pub prompt_id: String,
    pub displayed_copy_sha256: String,
    pub displayed_layout_sha256: String,
    pub instruction_revision: u64,
    pub acknowledged_host_time_ns: u64,
}

impl KeyboardLayoutConfirmationReceipt {
    #[must_use]
    pub fn us_qwerty(instruction_revision: u64, acknowledged_host_time_ns: u64) -> Self {
        Self {
            layout_id: US_QWERTY_LAYOUT_ID.to_owned(),
            prompt_id: US_QWERTY_CONFIRMATION_PROMPT_ID.to_owned(),
            displayed_copy_sha256: format_sha256(&sha256(US_QWERTY_CONFIRMATION_COPY.as_bytes())),
            displayed_layout_sha256: format_sha256(&sha256(&core28_layout_manifest_bytes())),
            instruction_revision,
            acknowledged_host_time_ns,
        }
    }

    pub(crate) fn validate(&self) -> Result<(), ArtifactError> {
        if self.layout_id != US_QWERTY_LAYOUT_ID
            || self.prompt_id != US_QWERTY_CONFIRMATION_PROMPT_ID
            || self.displayed_copy_sha256
                != format_sha256(&sha256(US_QWERTY_CONFIRMATION_COPY.as_bytes()))
            || self.displayed_layout_sha256
                != format_sha256(&sha256(&core28_layout_manifest_bytes()))
            || self.instruction_revision == 0
        {
            return Err(ArtifactError::InvalidKeyboardLayoutConfirmation);
        }
        Ok(())
    }
}

/// Selected physical keyboard identity retained with label provenance.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct KeyboardIdentity {
    pub name: String,
    pub physical_path: String,
    pub vendor_id: u16,
    pub product_id: u16,
    pub logical_layout: String,
    pub layout_confirmation: KeyboardLayoutConfirmationReceipt,
}

/// One hand's controller-time mapping.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HandClockFit {
    pub hand: Hand,
    pub fit: ClockFit,
}

/// One sensor's physical provenance, flattened in canonical hand/frame order.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactSensorBinding {
    pub hand: Hand,
    pub sensor: SensorManifest,
}

/// Every fixed decoder and segmentation value that changes live behavior.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DecoderContract {
    pub feature_count: usize,
    pub finger_energy_ratio: f64,
    pub distance_margin: f64,
    pub variance_floor: f64,
    pub max_inter_sensor_skew_us: i32,
    pub window_frames: usize,
    pub decision_delay_frames: u8,
    pub rearm_idle_windows: u8,
}

impl DecoderContract {
    #[must_use]
    pub const fn v1() -> Self {
        Self {
            feature_count: FEATURE_COUNT,
            finger_energy_ratio: FINGER_ENERGY_RATIO,
            distance_margin: DISTANCE_MARGIN,
            variance_floor: VARIANCE_FLOOR,
            max_inter_sensor_skew_us: MAX_INTER_SENSOR_SKEW_US,
            window_frames: WINDOW_FRAMES,
            decision_delay_frames: DECISION_DELAY_FRAMES,
            rearm_idle_windows: REARM_IDLE_WINDOWS,
        }
    }
}

/// Caller-owned evidence used to create one locked artifact.
pub struct CalibrationArtifactInput {
    pub acquisition_manifest_sha256: [u8; 32],
    pub prompt_corpus_sha256: [u8; 32],
    pub neutral_evidence_sha256: [u8; 32],
    pub calibration_matching_rule_sha256: [u8; 32],
    pub fitting_capture_journal_sha256: [u8; 32],
    pub fitting_evidence_sha256: [u8; 32],
    pub sensor_bindings: Vec<ArtifactSensorBinding>,
    pub keyboard: KeyboardIdentity,
    pub sources: Vec<ArtifactSource>,
    pub clock_fits: Vec<HandClockFit>,
    pub left_decoder: Option<HandDecoder>,
    pub right_decoder: Option<HandDecoder>,
    pub accepted_position_counts: Vec<u16>,
    pub rejected_examples: u32,
    pub setup_seconds: u32,
    pub created_unix_ns: u64,
    pub requires_wear_session_confirmation: bool,
}

/// Version 1 personal model package, locked before passive validation.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CalibrationArtifact {
    pub schema: String,
    pub layout_id: String,
    pub feature_schema: String,
    pub position_training_policy: String,
    pub acquisition_manifest_sha256: String,
    pub prompt_corpus_sha256: String,
    pub neutral_evidence_sha256: String,
    pub calibration_matching_rule_sha256: String,
    pub fitting_capture_journal_sha256: String,
    pub fitting_evidence_sha256: String,
    pub sensor_bindings: Vec<ArtifactSensorBinding>,
    pub keyboard: KeyboardIdentity,
    pub sources: Vec<ArtifactSource>,
    pub clock_fits: Vec<HandClockFit>,
    pub decoder_contract: DecoderContract,
    pub left_decoder: Option<HandDecoder>,
    pub right_decoder: Option<HandDecoder>,
    pub accepted_position_counts: Vec<u16>,
    pub rejected_examples: u32,
    pub setup_seconds: u32,
    pub created_unix_ns: u64,
    pub requires_wear_session_confirmation: bool,
}

impl CalibrationArtifact {
    /// Construct and validate a locked version 1 artifact.
    ///
    /// # Errors
    ///
    /// Returns the first missing provenance, topology, clock, or decoder
    /// invariant. No partial artifact is returned.
    pub fn new(input: CalibrationArtifactInput) -> Result<Self, ArtifactError> {
        let artifact = Self {
            schema: CALIBRATION_ARTIFACT_SCHEMA.to_owned(),
            layout_id: CORE28_LAYOUT_ID.to_owned(),
            feature_schema: FEATURE_SCHEMA.to_owned(),
            position_training_policy: POSITION_TRAINING_POLICY.to_owned(),
            acquisition_manifest_sha256: format_sha256(&input.acquisition_manifest_sha256),
            prompt_corpus_sha256: format_sha256(&input.prompt_corpus_sha256),
            neutral_evidence_sha256: format_sha256(&input.neutral_evidence_sha256),
            calibration_matching_rule_sha256: format_sha256(
                &input.calibration_matching_rule_sha256,
            ),
            fitting_capture_journal_sha256: format_sha256(&input.fitting_capture_journal_sha256),
            fitting_evidence_sha256: format_sha256(&input.fitting_evidence_sha256),
            sensor_bindings: input.sensor_bindings,
            keyboard: input.keyboard,
            sources: input.sources,
            clock_fits: input.clock_fits,
            decoder_contract: DecoderContract::v1(),
            left_decoder: input.left_decoder,
            right_decoder: input.right_decoder,
            accepted_position_counts: input.accepted_position_counts,
            rejected_examples: input.rejected_examples,
            setup_seconds: input.setup_seconds,
            created_unix_ns: input.created_unix_ns,
            requires_wear_session_confirmation: input.requires_wear_session_confirmation,
        };
        artifact.validate()?;
        Ok(artifact)
    }

    /// Parse and validate one bounded artifact.
    ///
    /// # Errors
    ///
    /// Rejects oversized, unknown-field, malformed, or invalid artifacts.
    pub fn from_json(source: &[u8]) -> Result<Self, ArtifactError> {
        if source.len() > MAX_ARTIFACT_BYTES {
            return Err(ArtifactError::TooLarge {
                actual: source.len(),
            });
        }
        let artifact = serde_json::from_slice::<Self>(source)
            .map_err(|error| ArtifactError::Json(error.to_string()))?;
        artifact.validate()?;
        Ok(artifact)
    }

    /// Validate a deserialized artifact before it reaches a runtime.
    ///
    /// # Errors
    ///
    /// Returns the first schema, provenance, or model inconsistency.
    pub fn validate(&self) -> Result<(), ArtifactError> {
        if self.schema != CALIBRATION_ARTIFACT_SCHEMA {
            return Err(ArtifactError::InvalidSchema);
        }
        if self.layout_id != CORE28_LAYOUT_ID {
            return Err(ArtifactError::InvalidLayout);
        }
        if self.feature_schema != FEATURE_SCHEMA || self.decoder_contract != DecoderContract::v1() {
            return Err(ArtifactError::DecoderContractMismatch);
        }
        if self.position_training_policy != POSITION_TRAINING_POLICY {
            return Err(ArtifactError::TrainingPolicyMismatch);
        }
        require_hash(
            &self.acquisition_manifest_sha256,
            "acquisition_manifest_sha256",
        )?;
        require_hash(&self.neutral_evidence_sha256, "neutral_evidence_sha256")?;
        require_hash(
            &self.calibration_matching_rule_sha256,
            "calibration_matching_rule_sha256",
        )?;
        require_hash(
            &self.fitting_capture_journal_sha256,
            "fitting_capture_journal_sha256",
        )?;
        require_hash(&self.fitting_evidence_sha256, "fitting_evidence_sha256")?;
        let prompt_hash = parse_sha256(&self.prompt_corpus_sha256)
            .ok_or(ArtifactError::InvalidSha256("prompt_corpus_sha256"))?;
        if prompt_hash != PromptCorpus::v1().sha256() {
            return Err(ArtifactError::PromptCorpusMismatch);
        }

        for (value, field) in [
            (&self.keyboard.name, "keyboard.name"),
            (&self.keyboard.physical_path, "keyboard.physical_path"),
            (&self.keyboard.logical_layout, "keyboard.logical_layout"),
        ] {
            require_identity(value, field)?;
        }
        if self.keyboard.logical_layout != US_QWERTY_LAYOUT_ID {
            return Err(ArtifactError::UnsupportedKeyboardLayout);
        }
        self.keyboard.layout_confirmation.validate()?;
        if self.keyboard.layout_confirmation.layout_id != self.keyboard.logical_layout {
            return Err(ArtifactError::InvalidKeyboardLayoutConfirmation);
        }

        let hands = self.configured_hands()?;
        if self.clock_fits.len() != hands.len() {
            return Err(ArtifactError::TopologyMismatch);
        }
        self.validate_sensor_bindings(&hands)?;
        for (expected, clock) in hands.iter().copied().zip(&self.clock_fits) {
            if clock.hand != expected {
                return Err(ArtifactError::TopologyMismatch);
            }
            clock.fit.validate().map_err(|error| ArtifactError::Clock {
                hand: clock.hand,
                error,
            })?;
        }

        let mut block_ids = BTreeSet::new();
        let mut has_keyboard_training = false;
        let mut has_surface_training = false;
        let mut has_passive_threshold = false;
        if self.sources.is_empty() {
            return Err(ArtifactError::MissingSources);
        }
        for source in &self.sources {
            require_hash(&source.session_sha256, "sources.session_sha256")?;
            if source.block_ids.is_empty() {
                return Err(ArtifactError::MissingSourceBlocks);
            }
            for &block_id in &source.block_ids {
                if !block_ids.insert(block_id) {
                    return Err(ArtifactError::OverlappingSourceBlock(block_id));
                }
            }
            match (source.role, source.domain) {
                (ArtifactSourceRole::Training, ArtifactDomain::Keyboard) => {
                    has_keyboard_training = true
                }
                (ArtifactSourceRole::Training, ArtifactDomain::PassiveSurface) => {
                    has_surface_training = true
                }
                (ArtifactSourceRole::ThresholdSelection, ArtifactDomain::PassiveSurface) => {
                    has_passive_threshold = true
                }
                (ArtifactSourceRole::ThresholdSelection, ArtifactDomain::Keyboard) => {
                    return Err(ArtifactError::InvalidSourcePolicy);
                }
            }
        }
        if !has_keyboard_training || !has_surface_training || !has_passive_threshold {
            return Err(ArtifactError::MissingSourceRole);
        }

        if self.accepted_position_counts.len() != PHYSICAL_POSITION_COUNT {
            return Err(ArtifactError::InvalidCoverageLength {
                actual: self.accepted_position_counts.len(),
            });
        }
        for (index, &count) in self.accepted_position_counts.iter().enumerate() {
            if count < MIN_ARTIFACT_POSITION_EXAMPLES {
                return Err(ArtifactError::IncompletePositionCoverage { index, count });
            }
        }
        if self.setup_seconds == 0
            || self.created_unix_ns == 0
            || !self.requires_wear_session_confirmation
        {
            return Err(ArtifactError::InvalidLifecycle);
        }
        Ok(())
    }

    fn validate_sensor_bindings(&self, hands: &[Hand]) -> Result<(), ArtifactError> {
        if self.sensor_bindings.len() != hands.len() * SENSORS_PER_HAND {
            return Err(ArtifactError::TopologyMismatch);
        }

        let mut mount_ids = BTreeSet::new();
        let mut wiring_ids = BTreeSet::new();
        for (hand_index, expected_hand) in hands.iter().copied().enumerate() {
            let start = hand_index * SENSORS_PER_HAND;
            let mut mux_channels = BTreeSet::new();
            let mut interrupt_pins = BTreeSet::new();
            for (sensor_position, expected_role) in
                CANONICAL_SENSOR_ROLES.iter().copied().enumerate()
            {
                let binding = &self.sensor_bindings[start + sensor_position];
                let sensor = &binding.sensor;
                if binding.hand != expected_hand
                    || usize::from(sensor.sensor_index) != sensor_position
                    || sensor.role != expected_role
                {
                    return Err(ArtifactError::TopologyMismatch);
                }
                for (identity, field) in [
                    (&sensor.mount_id, "sensor_bindings.sensor.mount_id"),
                    (&sensor.wiring_id, "sensor_bindings.sensor.wiring_id"),
                    (
                        &sensor.interrupt_pin,
                        "sensor_bindings.sensor.interrupt_pin",
                    ),
                ] {
                    require_identity(identity, field)?;
                }
                if !mount_ids.insert(sensor.mount_id.as_str()) {
                    return Err(ArtifactError::DuplicateIdentity(
                        "sensor_bindings.sensor.mount_id",
                    ));
                }
                if !wiring_ids.insert(sensor.wiring_id.as_str()) {
                    return Err(ArtifactError::DuplicateIdentity(
                        "sensor_bindings.sensor.wiring_id",
                    ));
                }
                if !interrupt_pins.insert(sensor.interrupt_pin.as_str()) {
                    return Err(ArtifactError::DuplicateIdentity(
                        "sensor_bindings.sensor.interrupt_pin",
                    ));
                }
                if !mux_channels.insert(sensor.mux_channel) || !is_proper_axis_map(sensor.axis_map)
                {
                    return Err(ArtifactError::InvalidSensorProvenance {
                        hand: expected_hand,
                        sensor_index: sensor.sensor_index,
                    });
                }
            }
        }
        Ok(())
    }

    /// Serialize the validated artifact to compact declaration-order JSON.
    ///
    /// # Errors
    ///
    /// Returns a validation or serializer error.
    pub fn canonical_json(&self) -> Result<Vec<u8>, ArtifactError> {
        self.validate()?;
        serde_json::to_vec(self).map_err(|error| ArtifactError::Serialization(error.to_string()))
    }

    /// Return SHA-256 of canonical artifact JSON.
    ///
    /// # Errors
    ///
    /// Returns a validation or serializer error.
    pub fn fingerprint(&self) -> Result<[u8; 32], ArtifactError> {
        Ok(sha256(&self.canonical_json()?))
    }

    /// Build the identity used to exclude fitting evidence from validation.
    ///
    /// # Errors
    ///
    /// Returns any artifact, hash, or source-identity error.
    pub fn locked_identity(&self) -> Result<LockedArtifactIdentity, ArtifactError> {
        let fingerprint = self.fingerprint()?;
        let manifest_sha256 = parse_sha256(&self.acquisition_manifest_sha256)
            .ok_or(ArtifactError::InvalidSha256("acquisition_manifest_sha256"))?;
        let source_sessions = self
            .sources
            .iter()
            .map(|source| {
                parse_sha256(&source.session_sha256)
                    .ok_or(ArtifactError::InvalidSha256("sources.session_sha256"))
            })
            .collect::<Result<BTreeSet<_>, _>>()?
            .into_iter()
            .collect::<Vec<_>>();
        LockedArtifactIdentity::new(fingerprint, manifest_sha256, source_sessions)
            .map_err(ArtifactError::Identity)
    }

    #[must_use]
    pub fn decoder(&self, hand: Hand) -> Option<&HandDecoder> {
        match hand {
            Hand::Left => self.left_decoder.as_ref(),
            Hand::Right => self.right_decoder.as_ref(),
        }
    }

    fn configured_hands(&self) -> Result<Vec<Hand>, ArtifactError> {
        let mut hands = Vec::with_capacity(2);
        if let Some(decoder) = &self.left_decoder {
            if decoder.hand() != Hand::Left {
                return Err(ArtifactError::TopologyMismatch);
            }
            decoder
                .validate_artifact()
                .map_err(|error| ArtifactError::Decoder {
                    hand: Hand::Left,
                    error,
                })?;
            hands.push(Hand::Left);
        }
        if let Some(decoder) = &self.right_decoder {
            if decoder.hand() != Hand::Right {
                return Err(ArtifactError::TopologyMismatch);
            }
            decoder
                .validate_artifact()
                .map_err(|error| ArtifactError::Decoder {
                    hand: Hand::Right,
                    error,
                })?;
            hands.push(Hand::Right);
        }
        if hands.is_empty() {
            return Err(ArtifactError::MissingDecoder);
        }
        Ok(hands)
    }
}

fn require_hash(value: &str, field: &'static str) -> Result<(), ArtifactError> {
    parse_sha256(value)
        .map(|_| ())
        .ok_or(ArtifactError::InvalidSha256(field))
}

fn require_identity(value: &str, field: &'static str) -> Result<(), ArtifactError> {
    if value.trim().is_empty() {
        return Err(ArtifactError::EmptyIdentity(field));
    }
    Ok(())
}

/// Why a persistent calibration artifact cannot enter live decoding.
#[derive(Clone, Debug, PartialEq)]
pub enum ArtifactError {
    TooLarge {
        actual: usize,
    },
    Json(String),
    Serialization(String),
    InvalidSchema,
    InvalidLayout,
    DecoderContractMismatch,
    TrainingPolicyMismatch,
    InvalidSha256(&'static str),
    PromptCorpusMismatch,
    EmptyIdentity(&'static str),
    DuplicateIdentity(&'static str),
    UnsupportedKeyboardLayout,
    InvalidKeyboardLayoutConfirmation,
    MissingDecoder,
    TopologyMismatch,
    InvalidSensorProvenance {
        hand: Hand,
        sensor_index: u8,
    },
    Decoder {
        hand: Hand,
        error: DecoderArtifactError,
    },
    Clock {
        hand: Hand,
        error: ClockError,
    },
    Identity(ArtifactIdentityError),
    MissingSources,
    MissingSourceBlocks,
    MissingSourceRole,
    InvalidSourcePolicy,
    OverlappingSourceBlock(u32),
    InvalidCoverageLength {
        actual: usize,
    },
    IncompletePositionCoverage {
        index: usize,
        count: u16,
    },
    InvalidLifecycle,
}

impl fmt::Display for ArtifactError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge { actual } => write!(
                formatter,
                "calibration artifact is {actual} bytes; maximum is {MAX_ARTIFACT_BYTES}"
            ),
            Self::Json(error) => write!(formatter, "calibration artifact JSON is invalid: {error}"),
            Self::Serialization(error) => {
                write!(
                    formatter,
                    "calibration artifact serialization failed: {error}"
                )
            }
            Self::InvalidSchema => formatter.write_str("calibration artifact schema is invalid"),
            Self::InvalidLayout => formatter.write_str("calibration artifact layout is invalid"),
            Self::DecoderContractMismatch => {
                formatter.write_str("calibration artifact decoder contract does not match v1")
            }
            Self::TrainingPolicyMismatch => formatter
                .write_str("calibration artifact position-training policy does not match v1"),
            Self::InvalidSha256(field) => {
                write!(formatter, "artifact field {field} is not SHA-256")
            }
            Self::PromptCorpusMismatch => {
                formatter.write_str("artifact prompt corpus does not match the v1 corpus")
            }
            Self::EmptyIdentity(field) => write!(formatter, "artifact field {field} is empty"),
            Self::DuplicateIdentity(field) => write!(formatter, "artifact repeats {field}"),
            Self::UnsupportedKeyboardLayout => {
                formatter.write_str("artifact keyboard layout is not US QWERTY v1")
            }
            Self::InvalidKeyboardLayoutConfirmation => formatter.write_str(
                "artifact keyboard layout is not bound to an explicit confirmation receipt",
            ),
            Self::MissingDecoder => formatter.write_str("artifact has no hand decoder"),
            Self::TopologyMismatch => {
                formatter.write_str("artifact decoder, clock, and sensor topology disagree")
            }
            Self::InvalidSensorProvenance { hand, sensor_index } => {
                write!(
                    formatter,
                    "artifact {hand:?} sensor {sensor_index} has invalid physical provenance"
                )
            }
            Self::Decoder { hand, error } => {
                write!(formatter, "artifact {hand:?} decoder is invalid: {error}")
            }
            Self::Clock { hand, error } => {
                write!(formatter, "artifact {hand:?} clock fit is invalid: {error}")
            }
            Self::Identity(error) => write!(formatter, "artifact identity is invalid: {error}"),
            Self::MissingSources => formatter.write_str("artifact has no source sessions"),
            Self::MissingSourceBlocks => {
                formatter.write_str("artifact source has no immutable block IDs")
            }
            Self::MissingSourceRole => formatter.write_str(
                "artifact needs keyboard warm-start, passive training, and passive threshold sources",
            ),
            Self::InvalidSourcePolicy => {
                formatter.write_str("artifact threshold selection must use passive-surface data")
            }
            Self::OverlappingSourceBlock(block) => {
                write!(formatter, "artifact reuses source block {block}")
            }
            Self::InvalidCoverageLength { actual } => write!(
                formatter,
                "artifact has {actual} position counts; expected {PHYSICAL_POSITION_COUNT}"
            ),
            Self::IncompletePositionCoverage { index, count } => write!(
                formatter,
                "artifact position {index} has {count} examples; expected at least {MIN_ARTIFACT_POSITION_EXAMPLES}"
            ),
            Self::InvalidLifecycle => {
                formatter.write_str("artifact lifecycle metadata is incomplete")
            }
        }
    }
}

impl Error for ArtifactError {}
