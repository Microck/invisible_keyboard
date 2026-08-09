use invisible_keyboard::{
    layout::Hand,
    protocol::{
        FRAME_LEN, FrameError, QUIESCE_RECEIPT_LEN, QUIESCE_REQUEST_LEN, QuiesceReceipt,
        QuiesceRecordError, QuiesceRequest, SENSOR_COUNT, SensorFrame, SensorReading, checksum,
        decode_frame, decode_quiesce_receipt, decode_quiesce_request, encode_frame,
        encode_quiesce_receipt, encode_quiesce_request,
    },
};

fn example_frame(hand: Hand) -> SensorFrame {
    let mut sensors = [SensorReading::default(); SENSOR_COUNT];
    for (index, sensor) in sensors.iter_mut().enumerate() {
        let base = i16::try_from(index * 100).expect("six sensors fit in i16");
        sensor.acceleration = [base - 3, base - 2, base - 1];
        sensor.angular_velocity = [base + 1, base + 2, base + 3];
        sensor.sample_time_offset_us = -base;
    }

    SensorFrame {
        hand,
        controller_id_sha256: [0x11; 32],
        boot_id: [0x22; 16],
        flags: 0,
        valid_sensor_mask: 0b00_111111,
        sequence: 0x1234_5678,
        device_time_us: 0x0102_0304_0506_0708,
        sensors,
    }
}

fn quiesce_request() -> QuiesceRequest {
    QuiesceRequest {
        hand: Hand::Left,
        controller_id_sha256: [0x11; 32],
        boot_id: [0x22; 16],
        session_id: [0x33; 16],
        request_start_time_ns: 0x0102_0304_0506_0708,
    }
}

fn quiesce_receipt() -> QuiesceReceipt {
    let request = quiesce_request();
    QuiesceReceipt {
        hand: request.hand,
        controller_id_sha256: request.controller_id_sha256,
        boot_id: request.boot_id,
        session_id: request.session_id,
        request_start_time_ns: request.request_start_time_ns,
        final_sequence: 0x1234_5678,
    }
}

fn rewrite_control_checksum(bytes: &mut [u8]) {
    let checksum_offset = bytes.len() - 4;
    let record_checksum = checksum(&bytes[..checksum_offset]);
    bytes[checksum_offset..].copy_from_slice(&record_checksum.to_le_bytes());
}

fn decode_hex<const N: usize>(source: &str) -> [u8; N] {
    assert_eq!(source.len(), N * 2);
    let mut decoded = [0_u8; N];
    for (index, byte) in decoded.iter_mut().enumerate() {
        *byte =
            u8::from_str_radix(&source[index * 2..index * 2 + 2], 16).expect("golden hex is valid");
    }
    decoded
}

#[test]
fn canonical_frame_is_exactly_160_bytes() {
    let encoded = encode_frame(&example_frame(Hand::Left)).expect("valid frame encodes");

    assert_eq!(FRAME_LEN, 160);
    assert_eq!(encoded.len(), FRAME_LEN);
    assert_eq!(&encoded[0..4], b"IKB2");
    assert_eq!(encoded[4], 2, "protocol version");
    assert_eq!(encoded[5], 0, "left hand wire value");
    assert_eq!(encoded[6], 6, "sensor count");
    assert_eq!(encoded[7], 0b00_111111, "sensor validity mask");
    assert_eq!(&encoded[12..16], &0x1234_5678_u32.to_le_bytes());
    assert_eq!(&encoded[16..24], &0x0102_0304_0506_0708_u64.to_le_bytes());
    assert_eq!(&encoded[24..56], &[0x11; 32], "controller identity");
    assert_eq!(&encoded[56..72], &[0x22; 16], "boot identity");
    assert_eq!(&encoded[72..74], &(-3_i16).to_le_bytes());
}

#[test]
fn round_trip_preserves_every_field_and_signed_extremes() {
    let mut frame = example_frame(Hand::Right);
    frame.sensors[0].acceleration = [i16::MIN, -1, 0];
    frame.sensors[0].sample_time_offset_us = i16::MIN;
    frame.sensors[5].angular_velocity = [1, i16::MAX, -32_000];
    frame.sensors[5].sample_time_offset_us = i16::MAX;

    let encoded = encode_frame(&frame).expect("valid frame encodes");
    let decoded = decode_frame(&encoded).expect("canonical frame decodes");

    assert_eq!(decoded, frame);
}

#[test]
fn every_single_bit_payload_corruption_is_rejected() {
    let encoded = encode_frame(&example_frame(Hand::Left)).expect("valid frame encodes");

    for byte_index in 0..FRAME_LEN - 4 {
        for bit_index in 0..8 {
            let mut corrupted = encoded;
            corrupted[byte_index] ^= 1 << bit_index;
            assert!(
                decode_frame(&corrupted).is_err(),
                "corruption at byte {byte_index}, bit {bit_index} was accepted"
            );
        }
    }
}

#[test]
fn truncated_and_extended_frames_are_rejected() {
    let encoded = encode_frame(&example_frame(Hand::Left)).expect("valid frame encodes");

    for length in 0..FRAME_LEN {
        assert!(matches!(
            decode_frame(&encoded[..length]),
            Err(FrameError::WrongLength { actual }) if actual == length
        ));
    }

    let mut extended = encoded.to_vec();
    extended.push(0);
    assert!(matches!(
        decode_frame(&extended),
        Err(FrameError::WrongLength { actual }) if actual == FRAME_LEN + 1
    ));
}

#[test]
fn left_and_right_have_distinct_wire_values() {
    let left = encode_frame(&example_frame(Hand::Left)).expect("left frame encodes");
    let right = encode_frame(&example_frame(Hand::Right)).expect("right frame encodes");

    assert_eq!(left[5], 0);
    assert_eq!(right[5], 1);
    assert_ne!(left, right);
}

#[test]
fn encoder_and_decoder_reject_missing_physical_identity() {
    let mut frame = example_frame(Hand::Left);
    frame.controller_id_sha256 = [0; 32];
    assert_eq!(
        encode_frame(&frame),
        Err(FrameError::MissingControllerIdentity)
    );

    let mut frame = example_frame(Hand::Left);
    frame.boot_id = [0; 16];
    assert_eq!(encode_frame(&frame), Err(FrameError::MissingBootIdentity));

    let mut encoded = encode_frame(&example_frame(Hand::Left)).expect("valid frame encodes");
    encoded[24..56].fill(0);
    let checksum = invisible_keyboard::protocol::checksum(&encoded[..FRAME_LEN - 4]);
    encoded[FRAME_LEN - 4..].copy_from_slice(&checksum.to_le_bytes());
    assert_eq!(
        decode_frame(&encoded),
        Err(FrameError::MissingControllerIdentity)
    );

    let mut encoded = encode_frame(&example_frame(Hand::Left)).expect("valid frame encodes");
    encoded[56..72].fill(0);
    let checksum = invisible_keyboard::protocol::checksum(&encoded[..FRAME_LEN - 4]);
    encoded[FRAME_LEN - 4..].copy_from_slice(&checksum.to_le_bytes());
    assert_eq!(decode_frame(&encoded), Err(FrameError::MissingBootIdentity));
}

#[test]
fn removed_fifo_status_bit_is_not_part_of_the_current_protocol() {
    let mut frame = example_frame(Hand::Left);
    frame.flags = 1_u32 << 0;
    assert_eq!(encode_frame(&frame), Err(FrameError::UnknownFlags(1)));

    let mut encoded = encode_frame(&example_frame(Hand::Left)).expect("valid frame encodes");
    encoded[8..12].copy_from_slice(&1_u32.to_le_bytes());
    let checksum = invisible_keyboard::protocol::checksum(&encoded[..FRAME_LEN - 4]);
    encoded[FRAME_LEN - 4..].copy_from_slice(&checksum.to_le_bytes());
    assert_eq!(decode_frame(&encoded), Err(FrameError::UnknownFlags(1)));
}

#[test]
fn quiesce_records_have_exact_layouts_and_round_trip() {
    let request = quiesce_request();
    let encoded_request = encode_quiesce_request(&request).expect("request encodes");
    let request_golden = decode_hex::<QUIESCE_REQUEST_LEN>(
        "494b5132020001001111111111111111111111111111111111111111111111111111111111111111\
         22222222222222222222222222222222333333333333333333333333333333330807060504030201\
         46da2f27",
    );
    assert_eq!(encoded_request.len(), QUIESCE_REQUEST_LEN);
    assert_eq!(encoded_request, request_golden);
    assert_eq!(&encoded_request[0..8], b"IKQ2\x02\x00\x01\x00");
    assert_eq!(&encoded_request[8..40], &[0x11; 32]);
    assert_eq!(&encoded_request[40..56], &[0x22; 16]);
    assert_eq!(&encoded_request[56..72], &[0x33; 16]);
    assert_eq!(
        &encoded_request[72..80],
        &0x0102_0304_0506_0708_u64.to_le_bytes()
    );
    assert_eq!(
        u32::from_le_bytes(encoded_request[80..84].try_into().expect("CRC bytes")),
        checksum(&encoded_request[..80])
    );
    assert_eq!(decode_quiesce_request(&encoded_request), Ok(request));

    let receipt = quiesce_receipt();
    let encoded_receipt = encode_quiesce_receipt(&receipt).expect("receipt encodes");
    let receipt_golden = decode_hex::<QUIESCE_RECEIPT_LEN>(
        "494b5232020001001111111111111111111111111111111111111111111111111111111111111111\
         22222222222222222222222222222222333333333333333333333333333333330807060504030201\
         785634125d98c9a6",
    );
    assert_eq!(encoded_receipt.len(), QUIESCE_RECEIPT_LEN);
    assert_eq!(encoded_receipt, receipt_golden);
    assert_eq!(&encoded_receipt[0..8], b"IKR2\x02\x00\x01\x00");
    assert_eq!(&encoded_receipt[80..84], &0x1234_5678_u32.to_le_bytes());
    assert_eq!(
        u32::from_le_bytes(encoded_receipt[84..88].try_into().expect("CRC bytes")),
        checksum(&encoded_receipt[..84])
    );
    assert_eq!(decode_quiesce_receipt(&encoded_receipt), Ok(receipt));
}

#[test]
fn quiesce_decoders_reject_lengths_checksums_and_checksum_valid_fixed_fields() {
    let request = encode_quiesce_request(&quiesce_request()).expect("request encodes");
    for length in 0..QUIESCE_REQUEST_LEN {
        assert!(matches!(
            decode_quiesce_request(&request[..length]),
            Err(QuiesceRecordError::WrongLength { actual, expected: QUIESCE_REQUEST_LEN })
                if actual == length
        ));
    }
    let mut extended = request.to_vec();
    extended.push(0);
    assert!(matches!(
        decode_quiesce_request(&extended),
        Err(QuiesceRecordError::WrongLength {
            actual,
            expected: QUIESCE_REQUEST_LEN,
        }) if actual == QUIESCE_REQUEST_LEN + 1
    ));

    for byte_index in 0..QUIESCE_REQUEST_LEN - 4 {
        for bit_index in 0..8 {
            let mut corrupted = request;
            corrupted[byte_index] ^= 1 << bit_index;
            assert!(matches!(
                decode_quiesce_request(&corrupted),
                Err(QuiesceRecordError::ChecksumMismatch { .. })
            ));
        }
    }
    let receipt = encode_quiesce_receipt(&quiesce_receipt()).expect("receipt encodes");
    for byte_index in 0..QUIESCE_RECEIPT_LEN - 4 {
        for bit_index in 0..8 {
            let mut corrupted = receipt;
            corrupted[byte_index] ^= 1 << bit_index;
            assert!(matches!(
                decode_quiesce_receipt(&corrupted),
                Err(QuiesceRecordError::ChecksumMismatch { .. })
            ));
        }
    }

    for (offset, value, expected) in [
        (4, 3, QuiesceRecordError::UnsupportedVersion(3)),
        (5, 2, QuiesceRecordError::InvalidHand(2)),
        (6, 2, QuiesceRecordError::InvalidDiscriminator(2)),
        (7, 1, QuiesceRecordError::NonzeroReserved(1)),
    ] {
        let mut invalid = request;
        invalid[offset] = value;
        rewrite_control_checksum(&mut invalid);
        assert_eq!(decode_quiesce_request(&invalid), Err(expected));
    }
    let mut wrong_magic = request;
    wrong_magic[0..4].copy_from_slice(b"NOPE");
    rewrite_control_checksum(&mut wrong_magic);
    assert_eq!(
        decode_quiesce_request(&wrong_magic),
        Err(QuiesceRecordError::BadMagic)
    );
}

#[test]
fn quiesce_encoders_and_decoders_reject_zero_identities() {
    let mut request = quiesce_request();
    request.controller_id_sha256 = [0; 32];
    assert_eq!(
        encode_quiesce_request(&request),
        Err(QuiesceRecordError::MissingControllerIdentity)
    );
    request = quiesce_request();
    request.boot_id = [0; 16];
    assert_eq!(
        encode_quiesce_request(&request),
        Err(QuiesceRecordError::MissingBootIdentity)
    );
    request = quiesce_request();
    request.session_id = [0; 16];
    assert_eq!(
        encode_quiesce_request(&request),
        Err(QuiesceRecordError::MissingSessionId)
    );

    let mut encoded = encode_quiesce_receipt(&quiesce_receipt()).expect("receipt encodes");
    for (range, expected) in [
        (8..40, QuiesceRecordError::MissingControllerIdentity),
        (40..56, QuiesceRecordError::MissingBootIdentity),
        (56..72, QuiesceRecordError::MissingSessionId),
    ] {
        let mut invalid = encoded;
        invalid[range].fill(0);
        rewrite_control_checksum(&mut invalid);
        assert_eq!(decode_quiesce_receipt(&invalid), Err(expected));
    }
    encoded[0] ^= 1;
    assert!(matches!(
        decode_quiesce_receipt(&encoded),
        Err(QuiesceRecordError::ChecksumMismatch { .. })
    ));
}
