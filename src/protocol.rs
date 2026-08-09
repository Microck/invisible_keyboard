//! Fixed-length, versioned, checksummed per-hand sensor frames.

use std::{error::Error, fmt};

use crate::layout::Hand;

/// Number of canonical sensor channels per hand.
pub const SENSOR_COUNT: usize = 6;

/// Encoded byte length of a version 2 sensor frame.
pub const FRAME_LEN: usize = 160;

/// Decoded byte length of one host-to-controller quiesce request.
pub const QUIESCE_REQUEST_LEN: usize = 84;

/// Decoded byte length of one controller-to-host quiesce receipt.
pub const QUIESCE_RECEIPT_LEN: usize = 88;

/// Current exact protocol version.
pub const PROTOCOL_VERSION: u8 = 2;

/// All six canonical sensors are valid.
pub const ALL_SENSORS_VALID: u8 = 0b00_111111;

/// The controller has not established an acceptable host clock fit.
pub const FLAG_CLOCK_UNSYNCED: u32 = 1 << 1;

/// Acquisition exceeded its scheduled frame period.
pub const FLAG_ACQUISITION_OVERRUN: u32 = 1 << 2;

const KNOWN_FLAGS: u32 = FLAG_CLOCK_UNSYNCED | FLAG_ACQUISITION_OVERRUN;
pub(crate) const MAGIC: &[u8; 4] = b"IKB2";
pub(crate) const QUIESCE_REQUEST_MAGIC: &[u8; 4] = b"IKQ2";
pub(crate) const QUIESCE_RECEIPT_MAGIC: &[u8; 4] = b"IKR2";
const QUIESCE_DISCRIMINATOR: u8 = 1;
const QUIESCE_IDENTITY_OFFSET: usize = 8;
const QUIESCE_BOOT_ID_OFFSET: usize = 40;
const QUIESCE_SESSION_ID_OFFSET: usize = 56;
const QUIESCE_REQUEST_TIME_OFFSET: usize = 72;
const QUIESCE_REQUEST_CHECKSUM_OFFSET: usize = QUIESCE_REQUEST_LEN - 4;
const QUIESCE_RECEIPT_SEQUENCE_OFFSET: usize = 80;
const QUIESCE_RECEIPT_CHECKSUM_OFFSET: usize = QUIESCE_RECEIPT_LEN - 4;
const SENSOR_OFFSET: usize = 72;
const CHECKSUM_OFFSET: usize = FRAME_LEN - 4;

/// Raw acceleration and angular-velocity channels from one IMU.
///
/// Scale and full-range settings belong to the immutable session manifest.
/// Keeping raw signed values avoids repeating `LapCal`'s ambiguous `g*` field
/// names for fused orientation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SensorReading {
    /// Raw acceleration X, Y, and Z.
    pub acceleration: [i16; 3],
    /// Raw angular velocity X, Y, and Z.
    pub angular_velocity: [i16; 3],
    /// Reference data-ready time minus aggregate time, in microseconds.
    /// The immutable session manifest identifies the hardware edge.
    pub sample_time_offset_us: i16,
}

/// One aggregate reading from the six canonical sensors on one hand.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SensorFrame {
    /// Hand that produced the frame.
    pub hand: Hand,
    /// SHA-256 of the manifest controller identity string.
    pub controller_id_sha256: [u8; 32],
    /// Random controller boot identity generated before acquisition starts.
    pub boot_id: [u8; 16],
    /// Version 2 status bits. Unknown bits are invalid on encode or decode.
    pub flags: u32,
    /// Bits 0 through 5 identify sensors whose readings are valid.
    pub valid_sensor_mask: u8,
    /// Wrapping per-hand sequence number.
    pub sequence: u32,
    /// Monotonic controller time since boot, in microseconds.
    pub device_time_us: u64,
    /// Canonical order: thumb, index, middle, ring, pinky, dorsal.
    pub sensors: [SensorReading; SENSOR_COUNT],
}

/// Physical controller identity repeated on every frame.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct FrameIdentity {
    pub hand: Hand,
    pub controller_id_sha256: [u8; 32],
    pub boot_id: [u8; 16],
}

/// One exact outbound `IKQ2` stop request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct QuiesceRequest {
    pub hand: Hand,
    pub controller_id_sha256: [u8; 32],
    pub boot_id: [u8; 16],
    /// Nonzero IKS3 session ID, which also serves as the request nonce.
    pub session_id: [u8; 16],
    /// Session-relative host time captured immediately before the first write.
    pub request_start_time_ns: u64,
}

/// One exact inbound `IKR2` success receipt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct QuiesceReceipt {
    pub hand: Hand,
    pub controller_id_sha256: [u8; 32],
    pub boot_id: [u8; 16],
    pub session_id: [u8; 16],
    pub request_start_time_ns: u64,
    /// Sequence of the final normal SensorFrame sent before this receipt.
    pub final_sequence: u32,
}

impl SensorFrame {
    /// Extract the immutable source identity without copying sensor samples.
    #[must_use]
    pub const fn identity(&self) -> FrameIdentity {
        FrameIdentity {
            hand: self.hand,
            controller_id_sha256: self.controller_id_sha256,
            boot_id: self.boot_id,
        }
    }
}

/// Reasons an untrusted frame cannot enter a session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrameError {
    /// The transport supplied a record with the wrong exact length.
    WrongLength { actual: usize },
    /// The frame does not start with the version 2 magic bytes.
    BadMagic,
    /// The frame version is not the one canonical current version.
    UnsupportedVersion(u8),
    /// The hand byte is neither left nor right.
    InvalidHand(u8),
    /// The fixed sensor count differs from six.
    InvalidSensorCount(u8),
    /// The validity mask sets bits outside the six sensor channels.
    InvalidSensorMask(u8),
    /// Flags contain bits that version 2 does not define.
    UnknownFlags(u32),
    /// The controller identity hash is all zero and therefore unauditable.
    MissingControllerIdentity,
    /// The boot identity is all zero and therefore cannot define an epoch.
    MissingBootIdentity,
    /// The stored CRC-32 does not match the preceding 156 bytes.
    ChecksumMismatch { stored: u32, calculated: u32 },
}

impl fmt::Display for FrameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongLength { actual } => {
                write!(
                    formatter,
                    "sensor frame is {actual} bytes; expected {FRAME_LEN}"
                )
            }
            Self::BadMagic => formatter.write_str("sensor frame has invalid magic"),
            Self::UnsupportedVersion(version) => {
                write!(formatter, "sensor frame version {version} is unsupported")
            }
            Self::InvalidHand(hand) => write!(formatter, "sensor frame hand {hand} is invalid"),
            Self::InvalidSensorCount(count) => {
                write!(
                    formatter,
                    "sensor frame declares {count} sensors; expected {SENSOR_COUNT}"
                )
            }
            Self::InvalidSensorMask(mask) => {
                write!(
                    formatter,
                    "sensor frame validity mask {mask:#010b} is invalid"
                )
            }
            Self::UnknownFlags(flags) => {
                write!(
                    formatter,
                    "sensor frame flags {flags:#010x} contain unknown bits"
                )
            }
            Self::MissingControllerIdentity => {
                formatter.write_str("sensor frame controller identity is all zero")
            }
            Self::MissingBootIdentity => {
                formatter.write_str("sensor frame boot identity is all zero")
            }
            Self::ChecksumMismatch { stored, calculated } => write!(
                formatter,
                "sensor frame checksum {stored:#010x} does not match {calculated:#010x}"
            ),
        }
    }
}

impl Error for FrameError {}

/// Reasons an exact quiesce request or receipt is not admissible.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QuiesceRecordError {
    WrongLength { actual: usize, expected: usize },
    BadMagic,
    UnsupportedVersion(u8),
    InvalidHand(u8),
    InvalidDiscriminator(u8),
    NonzeroReserved(u8),
    MissingControllerIdentity,
    MissingBootIdentity,
    MissingSessionId,
    ChecksumMismatch { stored: u32, calculated: u32 },
}

impl fmt::Display for QuiesceRecordError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongLength { actual, expected } => {
                write!(
                    formatter,
                    "quiesce record is {actual} bytes; expected {expected}"
                )
            }
            Self::BadMagic => formatter.write_str("quiesce record has invalid magic"),
            Self::UnsupportedVersion(version) => {
                write!(formatter, "quiesce record version {version} is unsupported")
            }
            Self::InvalidHand(hand) => write!(formatter, "quiesce record hand {hand} is invalid"),
            Self::InvalidDiscriminator(value) => write!(
                formatter,
                "quiesce record discriminator {value} is unsupported"
            ),
            Self::NonzeroReserved(value) => {
                write!(
                    formatter,
                    "quiesce record reserved byte is {value}; expected zero"
                )
            }
            Self::MissingControllerIdentity => {
                formatter.write_str("quiesce record controller identity is all zero")
            }
            Self::MissingBootIdentity => {
                formatter.write_str("quiesce record boot identity is all zero")
            }
            Self::MissingSessionId => formatter.write_str("quiesce record session ID is all zero"),
            Self::ChecksumMismatch { stored, calculated } => write!(
                formatter,
                "quiesce record checksum {stored:#010x} does not match {calculated:#010x}"
            ),
        }
    }
}

impl Error for QuiesceRecordError {}

/// Encode one canonical version 2 sensor frame.
///
/// # Errors
///
/// Returns [`FrameError`] when a caller tries to emit invalid status bits,
/// sensor validity bits, or an all-zero physical identity.
pub fn encode_frame(frame: &SensorFrame) -> Result<[u8; FRAME_LEN], FrameError> {
    validate_invariants(frame.valid_sensor_mask, frame.flags)?;
    validate_identity(&frame.controller_id_sha256, &frame.boot_id)?;

    let mut bytes = [0_u8; FRAME_LEN];
    bytes[0..4].copy_from_slice(MAGIC);
    bytes[4] = PROTOCOL_VERSION;
    bytes[5] = frame.hand as u8;
    bytes[6] = SENSOR_COUNT as u8;
    bytes[7] = frame.valid_sensor_mask;
    bytes[8..12].copy_from_slice(&frame.flags.to_le_bytes());
    bytes[12..16].copy_from_slice(&frame.sequence.to_le_bytes());
    bytes[16..24].copy_from_slice(&frame.device_time_us.to_le_bytes());
    bytes[24..56].copy_from_slice(&frame.controller_id_sha256);
    bytes[56..72].copy_from_slice(&frame.boot_id);

    let mut offset = SENSOR_OFFSET;
    for sensor in frame.sensors {
        for axis in sensor.acceleration {
            bytes[offset..offset + 2].copy_from_slice(&axis.to_le_bytes());
            offset += 2;
        }
        for axis in sensor.angular_velocity {
            bytes[offset..offset + 2].copy_from_slice(&axis.to_le_bytes());
            offset += 2;
        }
        bytes[offset..offset + 2].copy_from_slice(&sensor.sample_time_offset_us.to_le_bytes());
        offset += 2;
    }
    debug_assert_eq!(offset, CHECKSUM_OFFSET);

    let checksum = checksum(&bytes[..CHECKSUM_OFFSET]);
    bytes[CHECKSUM_OFFSET..FRAME_LEN].copy_from_slice(&checksum.to_le_bytes());
    Ok(bytes)
}

/// Decode and validate one untrusted version 2 sensor frame.
///
/// # Errors
///
/// Returns [`FrameError`] when the record has the wrong exact length, a bad
/// checksum, or any header value outside the one canonical protocol version.
pub fn decode_frame(bytes: &[u8]) -> Result<SensorFrame, FrameError> {
    if bytes.len() != FRAME_LEN {
        return Err(FrameError::WrongLength {
            actual: bytes.len(),
        });
    }

    let stored_checksum = read_u32(bytes, CHECKSUM_OFFSET);
    let calculated_checksum = checksum(&bytes[..CHECKSUM_OFFSET]);
    if stored_checksum != calculated_checksum {
        return Err(FrameError::ChecksumMismatch {
            stored: stored_checksum,
            calculated: calculated_checksum,
        });
    }
    if &bytes[0..4] != MAGIC {
        return Err(FrameError::BadMagic);
    }
    if bytes[4] != PROTOCOL_VERSION {
        return Err(FrameError::UnsupportedVersion(bytes[4]));
    }
    let hand = Hand::try_from(bytes[5]).map_err(FrameError::InvalidHand)?;
    if usize::from(bytes[6]) != SENSOR_COUNT {
        return Err(FrameError::InvalidSensorCount(bytes[6]));
    }
    let flags = read_u32(bytes, 8);
    validate_invariants(bytes[7], flags)?;

    let sequence = read_u32(bytes, 12);
    let device_time_us = read_u64(bytes, 16);
    let controller_id_sha256 = bytes[24..56].try_into().expect("32-byte identity slice");
    let boot_id = bytes[56..72].try_into().expect("16-byte identity slice");
    validate_identity(&controller_id_sha256, &boot_id)?;
    let mut sensors = [SensorReading::default(); SENSOR_COUNT];
    let mut offset = SENSOR_OFFSET;
    for sensor in &mut sensors {
        for axis in &mut sensor.acceleration {
            *axis = read_i16(bytes, offset);
            offset += 2;
        }
        for axis in &mut sensor.angular_velocity {
            *axis = read_i16(bytes, offset);
            offset += 2;
        }
        sensor.sample_time_offset_us = read_i16(bytes, offset);
        offset += 2;
    }
    debug_assert_eq!(offset, CHECKSUM_OFFSET);

    Ok(SensorFrame {
        hand,
        controller_id_sha256,
        boot_id,
        flags,
        valid_sensor_mask: bytes[7],
        sequence,
        device_time_us,
        sensors,
    })
}

/// Encode one exact host-to-controller `IKQ2` request.
///
/// # Errors
///
/// Returns [`QuiesceRecordError`] when the physical identity or session ID is
/// all zero. Every other header field is fixed by this encoder.
pub fn encode_quiesce_request(
    request: &QuiesceRequest,
) -> Result<[u8; QUIESCE_REQUEST_LEN], QuiesceRecordError> {
    validate_quiesce_identity(
        &request.controller_id_sha256,
        &request.boot_id,
        &request.session_id,
    )?;

    let mut bytes = [0_u8; QUIESCE_REQUEST_LEN];
    encode_quiesce_header(
        &mut bytes,
        QUIESCE_REQUEST_MAGIC,
        request.hand,
        &request.controller_id_sha256,
        &request.boot_id,
        &request.session_id,
        request.request_start_time_ns,
    );
    let record_checksum = checksum(&bytes[..QUIESCE_REQUEST_CHECKSUM_OFFSET]);
    bytes[QUIESCE_REQUEST_CHECKSUM_OFFSET..].copy_from_slice(&record_checksum.to_le_bytes());
    Ok(bytes)
}

/// Decode and validate one exact host-to-controller `IKQ2` request.
///
/// # Errors
///
/// Returns [`QuiesceRecordError`] for any wrong length, checksum, fixed field,
/// hand value, or zero identity.
pub fn decode_quiesce_request(bytes: &[u8]) -> Result<QuiesceRequest, QuiesceRecordError> {
    let hand = validate_quiesce_record(
        bytes,
        QUIESCE_REQUEST_LEN,
        QUIESCE_REQUEST_MAGIC,
        QUIESCE_REQUEST_CHECKSUM_OFFSET,
    )?;
    let request = QuiesceRequest {
        hand,
        controller_id_sha256: bytes[QUIESCE_IDENTITY_OFFSET..QUIESCE_BOOT_ID_OFFSET]
            .try_into()
            .expect("32-byte identity slice"),
        boot_id: bytes[QUIESCE_BOOT_ID_OFFSET..QUIESCE_SESSION_ID_OFFSET]
            .try_into()
            .expect("16-byte boot identity slice"),
        session_id: bytes[QUIESCE_SESSION_ID_OFFSET..QUIESCE_REQUEST_TIME_OFFSET]
            .try_into()
            .expect("16-byte session identity slice"),
        request_start_time_ns: read_u64(bytes, QUIESCE_REQUEST_TIME_OFFSET),
    };
    validate_quiesce_identity(
        &request.controller_id_sha256,
        &request.boot_id,
        &request.session_id,
    )?;
    Ok(request)
}

/// Encode one exact controller-to-host `IKR2` success receipt.
///
/// # Errors
///
/// Returns [`QuiesceRecordError`] when the echoed physical identity or session
/// ID is all zero. Every other header field is fixed by this encoder.
pub fn encode_quiesce_receipt(
    receipt: &QuiesceReceipt,
) -> Result<[u8; QUIESCE_RECEIPT_LEN], QuiesceRecordError> {
    validate_quiesce_identity(
        &receipt.controller_id_sha256,
        &receipt.boot_id,
        &receipt.session_id,
    )?;

    let mut bytes = [0_u8; QUIESCE_RECEIPT_LEN];
    encode_quiesce_header(
        &mut bytes,
        QUIESCE_RECEIPT_MAGIC,
        receipt.hand,
        &receipt.controller_id_sha256,
        &receipt.boot_id,
        &receipt.session_id,
        receipt.request_start_time_ns,
    );
    bytes[QUIESCE_RECEIPT_SEQUENCE_OFFSET..QUIESCE_RECEIPT_CHECKSUM_OFFSET]
        .copy_from_slice(&receipt.final_sequence.to_le_bytes());
    let record_checksum = checksum(&bytes[..QUIESCE_RECEIPT_CHECKSUM_OFFSET]);
    bytes[QUIESCE_RECEIPT_CHECKSUM_OFFSET..].copy_from_slice(&record_checksum.to_le_bytes());
    Ok(bytes)
}

/// Decode and validate one exact controller-to-host `IKR2` receipt.
///
/// # Errors
///
/// Returns [`QuiesceRecordError`] for any wrong length, checksum, fixed field,
/// hand value, or zero identity.
pub fn decode_quiesce_receipt(bytes: &[u8]) -> Result<QuiesceReceipt, QuiesceRecordError> {
    let hand = validate_quiesce_record(
        bytes,
        QUIESCE_RECEIPT_LEN,
        QUIESCE_RECEIPT_MAGIC,
        QUIESCE_RECEIPT_CHECKSUM_OFFSET,
    )?;
    let receipt = QuiesceReceipt {
        hand,
        controller_id_sha256: bytes[QUIESCE_IDENTITY_OFFSET..QUIESCE_BOOT_ID_OFFSET]
            .try_into()
            .expect("32-byte identity slice"),
        boot_id: bytes[QUIESCE_BOOT_ID_OFFSET..QUIESCE_SESSION_ID_OFFSET]
            .try_into()
            .expect("16-byte boot identity slice"),
        session_id: bytes[QUIESCE_SESSION_ID_OFFSET..QUIESCE_REQUEST_TIME_OFFSET]
            .try_into()
            .expect("16-byte session identity slice"),
        request_start_time_ns: read_u64(bytes, QUIESCE_REQUEST_TIME_OFFSET),
        final_sequence: read_u32(bytes, QUIESCE_RECEIPT_SEQUENCE_OFFSET),
    };
    validate_quiesce_identity(
        &receipt.controller_id_sha256,
        &receipt.boot_id,
        &receipt.session_id,
    )?;
    Ok(receipt)
}

fn encode_quiesce_header(
    bytes: &mut [u8],
    magic: &[u8; 4],
    hand: Hand,
    controller_id_sha256: &[u8; 32],
    boot_id: &[u8; 16],
    session_id: &[u8; 16],
    request_start_time_ns: u64,
) {
    bytes[0..4].copy_from_slice(magic);
    bytes[4] = PROTOCOL_VERSION;
    bytes[5] = hand as u8;
    bytes[6] = QUIESCE_DISCRIMINATOR;
    bytes[QUIESCE_IDENTITY_OFFSET..QUIESCE_BOOT_ID_OFFSET].copy_from_slice(controller_id_sha256);
    bytes[QUIESCE_BOOT_ID_OFFSET..QUIESCE_SESSION_ID_OFFSET].copy_from_slice(boot_id);
    bytes[QUIESCE_SESSION_ID_OFFSET..QUIESCE_REQUEST_TIME_OFFSET].copy_from_slice(session_id);
    bytes[QUIESCE_REQUEST_TIME_OFFSET..QUIESCE_REQUEST_TIME_OFFSET + 8]
        .copy_from_slice(&request_start_time_ns.to_le_bytes());
}

fn validate_quiesce_record(
    bytes: &[u8],
    expected_len: usize,
    magic: &[u8; 4],
    checksum_offset: usize,
) -> Result<Hand, QuiesceRecordError> {
    if bytes.len() != expected_len {
        return Err(QuiesceRecordError::WrongLength {
            actual: bytes.len(),
            expected: expected_len,
        });
    }
    let stored = read_u32(bytes, checksum_offset);
    let calculated = checksum(&bytes[..checksum_offset]);
    if stored != calculated {
        return Err(QuiesceRecordError::ChecksumMismatch { stored, calculated });
    }
    if &bytes[0..4] != magic {
        return Err(QuiesceRecordError::BadMagic);
    }
    if bytes[4] != PROTOCOL_VERSION {
        return Err(QuiesceRecordError::UnsupportedVersion(bytes[4]));
    }
    let hand = Hand::try_from(bytes[5]).map_err(QuiesceRecordError::InvalidHand)?;
    if bytes[6] != QUIESCE_DISCRIMINATOR {
        return Err(QuiesceRecordError::InvalidDiscriminator(bytes[6]));
    }
    if bytes[7] != 0 {
        return Err(QuiesceRecordError::NonzeroReserved(bytes[7]));
    }
    Ok(hand)
}

fn validate_quiesce_identity(
    controller_id_sha256: &[u8; 32],
    boot_id: &[u8; 16],
    session_id: &[u8; 16],
) -> Result<(), QuiesceRecordError> {
    if *controller_id_sha256 == [0; 32] {
        return Err(QuiesceRecordError::MissingControllerIdentity);
    }
    if *boot_id == [0; 16] {
        return Err(QuiesceRecordError::MissingBootIdentity);
    }
    if *session_id == [0; 16] {
        return Err(QuiesceRecordError::MissingSessionId);
    }
    Ok(())
}

fn validate_invariants(valid_sensor_mask: u8, flags: u32) -> Result<(), FrameError> {
    if valid_sensor_mask & !ALL_SENSORS_VALID != 0 {
        return Err(FrameError::InvalidSensorMask(valid_sensor_mask));
    }
    if flags & !KNOWN_FLAGS != 0 {
        return Err(FrameError::UnknownFlags(flags));
    }
    Ok(())
}

fn validate_identity(
    controller_id_sha256: &[u8; 32],
    boot_id: &[u8; 16],
) -> Result<(), FrameError> {
    if *controller_id_sha256 == [0; 32] {
        return Err(FrameError::MissingControllerIdentity);
    }
    if *boot_id == [0; 16] {
        return Err(FrameError::MissingBootIdentity);
    }
    Ok(())
}

fn read_i16(bytes: &[u8], offset: usize) -> i16 {
    i16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
        bytes[offset + 4],
        bytes[offset + 5],
        bytes[offset + 6],
        bytes[offset + 7],
    ])
}

// IEEE CRC-32, reflected polynomial 0xedb88320. This small table-free form is
// fast enough for a 160-byte reference frame and keeps the protocol fixture
// independent of a host-only dependency.
#[must_use]
pub fn checksum(bytes: &[u8]) -> u32 {
    checksum_parts(&[bytes])
}

pub(crate) fn checksum_parts(parts: &[&[u8]]) -> u32 {
    let mut crc = u32::MAX;
    for bytes in parts {
        for byte in *bytes {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                let mask = 0_u32.wrapping_sub(crc & 1);
                crc = (crc >> 1) ^ (0xedb8_8320 & mask);
            }
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rewrite_checksum(bytes: &mut [u8; FRAME_LEN]) {
        let checksum = checksum(&bytes[..CHECKSUM_OFFSET]);
        bytes[CHECKSUM_OFFSET..].copy_from_slice(&checksum.to_le_bytes());
    }

    fn valid_frame() -> SensorFrame {
        SensorFrame {
            hand: Hand::Left,
            controller_id_sha256: [0x11; 32],
            boot_id: [0x22; 16],
            flags: 0,
            valid_sensor_mask: ALL_SENSORS_VALID,
            sequence: 7,
            device_time_us: 5_000,
            sensors: [SensorReading::default(); SENSOR_COUNT],
        }
    }

    #[test]
    fn crc32_matches_standard_check_value() {
        assert_eq!(checksum(b"123456789"), 0xcbf4_3926);
    }

    #[test]
    fn checksum_valid_invalid_headers_are_rejected_semantically() {
        let cases = [
            (4, 3, FrameError::UnsupportedVersion(3)),
            (5, 2, FrameError::InvalidHand(2)),
            (6, 7, FrameError::InvalidSensorCount(7)),
            (7, 0b0100_0000, FrameError::InvalidSensorMask(0b0100_0000)),
        ];

        for (offset, value, expected) in cases {
            let mut bytes = encode_frame(&valid_frame()).expect("valid frame encodes");
            bytes[offset] = value;
            rewrite_checksum(&mut bytes);
            assert_eq!(decode_frame(&bytes), Err(expected));
        }
    }

    #[test]
    fn checksum_valid_unknown_flags_are_rejected() {
        for unknown in [1_u32 << 0, 1_u32 << 31] {
            let mut bytes = encode_frame(&valid_frame()).expect("valid frame encodes");
            bytes[8..12].copy_from_slice(&unknown.to_le_bytes());
            rewrite_checksum(&mut bytes);

            assert_eq!(decode_frame(&bytes), Err(FrameError::UnknownFlags(unknown)));
        }
    }

    #[test]
    fn checksum_valid_bad_magic_is_rejected() {
        let mut bytes = encode_frame(&valid_frame()).expect("valid frame encodes");
        bytes[0..4].copy_from_slice(b"NOPE");
        rewrite_checksum(&mut bytes);

        assert_eq!(decode_frame(&bytes), Err(FrameError::BadMagic));
    }
}
