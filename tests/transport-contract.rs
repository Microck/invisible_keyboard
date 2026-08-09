use invisible_keyboard::{
    layout::Hand,
    protocol::{
        ALL_SENSORS_VALID, QuiesceReceipt, QuiesceRequest, SENSOR_COUNT, SensorFrame,
        SensorReading, decode_frame, encode_frame, encode_quiesce_receipt,
    },
    transport::{
        ControllerRecord, ControllerRecordError, MAX_ENCODED_FRAME_LEN, MAX_TRANSPORT_RECORD_LEN,
        QUIESCE_RECEIPT_TRANSPORT_LEN, QUIESCE_REQUEST_TRANSPORT_LEN, TransportCandidate,
        TransportDecoder, decode_controller_record, encode_transport_frame,
        encode_transport_quiesce_receipt, encode_transport_quiesce_request,
    },
};

fn frame(sequence: u32) -> SensorFrame {
    SensorFrame {
        hand: Hand::Left,
        controller_id_sha256: [0x11; 32],
        boot_id: [0x31; 16],
        flags: 0,
        valid_sensor_mask: ALL_SENSORS_VALID,
        sequence,
        device_time_us: u64::from(sequence) * 5_000,
        sensors: [SensorReading::default(); SENSOR_COUNT],
    }
}

fn quiesce_request() -> QuiesceRequest {
    QuiesceRequest {
        hand: Hand::Left,
        controller_id_sha256: [0x11; 32],
        boot_id: [0x31; 16],
        session_id: [0x41; 16],
        request_start_time_ns: 900,
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
        final_sequence: 77,
    }
}

fn encoded_transport(frame: &SensorFrame) -> Vec<u8> {
    encode_transport_frame(frame).expect("valid frame encodes")
}

#[test]
fn every_chunk_boundary_reconstructs_one_exact_frame() {
    let expected = encode_frame(&frame(7)).expect("valid frame encodes");
    let transport = encoded_transport(&frame(7));

    assert_eq!(transport.last(), Some(&0));
    assert!(transport.len() <= MAX_TRANSPORT_RECORD_LEN);

    for split in 0..=transport.len() {
        let mut decoder = TransportDecoder::new();
        let mut candidates = decoder.push(&transport[..split]);
        candidates.extend(decoder.push(&transport[split..]));
        assert_eq!(
            candidates,
            vec![TransportCandidate::Decoded(expected.to_vec())]
        );
        assert_eq!(decoder.finish(), None);
    }
}

#[test]
fn malformed_record_does_not_shift_the_next_record() {
    let mut stream = vec![5, 1, 2, 0];
    stream.extend(encoded_transport(&frame(9)));

    let mut decoder = TransportDecoder::new();
    let candidates = decoder.push(&stream);

    assert!(matches!(
        &candidates[0],
        TransportCandidate::Malformed { encoded } if encoded == &[5, 1, 2]
    ));
    let TransportCandidate::Decoded(recovered) = &candidates[1] else {
        panic!("second candidate must recover at the next delimiter");
    };
    assert_eq!(
        decode_frame(recovered).expect("recovered frame").sequence,
        9
    );
}

#[test]
fn oversized_record_is_bounded_and_next_record_recovers() {
    let mut stream = vec![1; MAX_ENCODED_FRAME_LEN + 17];
    stream.push(0);
    stream.extend(encoded_transport(&frame(11)));

    let mut decoder = TransportDecoder::new();
    let candidates = decoder.push(&stream);

    assert!(matches!(
        &candidates[0],
        TransportCandidate::Oversized { prefix, actual_len }
            if prefix.len() == MAX_ENCODED_FRAME_LEN
                && *actual_len == MAX_ENCODED_FRAME_LEN + 17
    ));
    let TransportCandidate::Decoded(recovered) = &candidates[1] else {
        panic!("valid record after oversized input must recover");
    };
    assert_eq!(
        decode_frame(recovered).expect("recovered frame").sequence,
        11
    );
}

#[test]
fn disconnect_preserves_an_unterminated_candidate() {
    let transport = encoded_transport(&frame(13));
    let mut decoder = TransportDecoder::new();
    assert!(decoder.push(&transport[..transport.len() - 1]).is_empty());

    assert!(matches!(
        decoder.finish(),
        Some(TransportCandidate::Unterminated { encoded }) if !encoded.is_empty()
    ));
    assert_eq!(decoder.finish(), None, "finish drains the candidate once");
}

#[test]
fn empty_delimiters_are_ignored() {
    let mut decoder = TransportDecoder::new();
    assert!(decoder.push(&[0, 0, 0]).is_empty());
    assert_eq!(decoder.finish(), None);
}

#[test]
fn every_single_byte_deletion_recovers_by_the_third_frame() {
    let first = encoded_transport(&frame(21));
    let second = encoded_transport(&frame(22));
    let third = encoded_transport(&frame(23));

    for deleted in 0..first.len() {
        let mut stream = first.clone();
        stream.remove(deleted);
        stream.extend(&second);
        stream.extend(&third);

        let mut decoder = TransportDecoder::new();
        let recovered = decoder
            .push(&stream)
            .into_iter()
            .filter_map(|candidate| match candidate {
                TransportCandidate::Decoded(payload) => decode_frame(&payload).ok(),
                _ => None,
            })
            .map(|frame| frame.sequence)
            .collect::<Vec<_>>();

        assert!(
            recovered.contains(&23),
            "deleting transport byte {deleted} prevented delimiter recovery: {recovered:?}"
        );
        assert_eq!(decoder.finish(), None);
    }
}

#[test]
fn inserted_zero_and_nonzero_bytes_recover_by_the_next_clean_frame() {
    let first = encoded_transport(&frame(31));
    let sentinel = encoded_transport(&frame(32));

    for inserted in 0..first.len() {
        for value in [0_u8, 1, u8::MAX] {
            let mut stream = first.clone();
            stream.insert(inserted, value);
            stream.extend(&sentinel);

            let mut decoder = TransportDecoder::new();
            let recovered = decoder
                .push(&stream)
                .into_iter()
                .filter_map(|candidate| match candidate {
                    TransportCandidate::Decoded(payload) => decode_frame(&payload).ok(),
                    _ => None,
                })
                .map(|frame| frame.sequence)
                .collect::<Vec<_>>();

            assert!(
                recovered.contains(&32),
                "inserting {value} at byte {inserted} prevented recovery: {recovered:?}"
            );
            assert_eq!(decoder.finish(), None);
        }
    }
}

#[test]
fn control_records_have_exact_cobs_lengths_and_survive_every_chunk_boundary() {
    let records = [
        (
            encode_transport_quiesce_request(&quiesce_request()).expect("request encodes"),
            QUIESCE_REQUEST_TRANSPORT_LEN,
        ),
        (
            encode_transport_quiesce_receipt(&quiesce_receipt()).expect("receipt encodes"),
            QUIESCE_RECEIPT_TRANSPORT_LEN,
        ),
    ];

    for (transport, expected_length) in records {
        assert_eq!(transport.len(), expected_length);
        assert_eq!(transport.last(), Some(&0));
        for split in 0..=transport.len() {
            let mut decoder = TransportDecoder::new();
            let mut candidates = decoder.push(&transport[..split]);
            candidates.extend(decoder.push(&transport[split..]));
            assert!(matches!(
                candidates.as_slice(),
                [TransportCandidate::Decoded(_)]
            ));
            assert_eq!(decoder.finish(), None);
        }
    }
}

#[test]
fn controller_dispatch_accepts_frames_and_receipts_but_rejects_outbound_requests() {
    let sensor = encode_frame(&frame(55)).expect("frame encodes");
    assert!(matches!(
        decode_controller_record(&sensor),
        Ok(ControllerRecord::SensorFrame(sensor_frame)) if sensor_frame.sequence == 55
    ));

    let receipt = quiesce_receipt();
    let encoded_receipt = encode_quiesce_receipt(&receipt).expect("receipt encodes");
    assert_eq!(
        decode_controller_record(&encoded_receipt),
        Ok(ControllerRecord::QuiesceReceipt(receipt))
    );

    let request_transport =
        encode_transport_quiesce_request(&quiesce_request()).expect("request encodes");
    let mut decoder = TransportDecoder::new();
    let candidates = decoder.push(&request_transport);
    let [TransportCandidate::Decoded(request)] = candidates.as_slice() else {
        panic!("request COBS decodes");
    };
    assert_eq!(
        decode_controller_record(request),
        Err(ControllerRecordError::WrongDirection)
    );
    assert_eq!(
        decode_controller_record(b"NOPE"),
        Err(ControllerRecordError::UnknownMagic)
    );
}

#[test]
fn partial_and_oversized_control_candidates_fail_without_shifting_the_next_record() {
    let request = encode_transport_quiesce_request(&quiesce_request()).expect("request encodes");
    let receipt = encode_transport_quiesce_receipt(&quiesce_receipt()).expect("receipt encodes");

    let mut partial = TransportDecoder::new();
    assert!(partial.push(&request[..request.len() - 1]).is_empty());
    assert!(matches!(
        partial.finish(),
        Some(TransportCandidate::Unterminated { encoded }) if !encoded.is_empty()
    ));

    let mut stream = vec![1; MAX_ENCODED_FRAME_LEN + 1];
    stream.push(0);
    stream.extend_from_slice(&receipt);
    let mut decoder = TransportDecoder::new();
    let recovered = decoder.push(&stream);
    assert!(matches!(
        &recovered[0],
        TransportCandidate::Oversized { actual_len, .. }
            if *actual_len == MAX_ENCODED_FRAME_LEN + 1
    ));
    let TransportCandidate::Decoded(receipt_bytes) = &recovered[1] else {
        panic!("receipt after oversize must recover");
    };
    assert_eq!(
        decode_controller_record(receipt_bytes),
        Ok(ControllerRecord::QuiesceReceipt(quiesce_receipt()))
    );
}
