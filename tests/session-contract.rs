use invisible_keyboard::{
    layout::Hand,
    protocol::{
        ALL_SENSORS_VALID, FLAG_CLOCK_UNSYNCED, FRAME_LEN, FrameIdentity, QuiesceReceipt,
        QuiesceRequest, SENSOR_COUNT, SensorFrame, SensorReading,
    },
    session::{
        ACQUISITION_RECORDS_PER_SECOND_PER_HAND, CORE28_MANIFEST_SHA256, ControllerStream,
        FOOTER_LEN, FinalizedSession, GATE0A_CAPTURE_TIME_SECONDS, GATE0A_HOST_RESPONSE_TIMEOUT_NS,
        Gate0aTerminalError, HEADER_LEN, MAX_ACQUISITION_RECORD_BYTES, MAX_GATE0A_SESSION_BYTES,
        MAX_GATE0A_SESSION_RECORDS, MAX_SETUP_SESSION_BYTES, MAX_SETUP_SESSION_RECORDS,
        RECORD_PREFIX_LEN, RecordDecodeError, SessionError, SessionHeader, SessionRecordKind,
        SessionWriter, audit_gate0a_session_reader, audit_session_reader,
        validate_calibration_session_size, validate_gate0a_session_size, write_reference_session,
    },
};

fn header() -> SessionHeader {
    SessionHeader {
        nominal_period_us: 5_000,
        manifest_sha256: [0x5a; 32],
        session_id: [0xa5; 16],
    }
}

fn quiesce_request() -> QuiesceRequest {
    QuiesceRequest {
        hand: Hand::Left,
        controller_id_sha256: [0x11; 32],
        boot_id: [0x31; 16],
        session_id: header().session_id,
        request_start_time_ns: 100,
    }
}

fn quiesce_receipt(final_sequence: u32) -> QuiesceReceipt {
    let request = quiesce_request();
    QuiesceReceipt {
        hand: request.hand,
        controller_id_sha256: request.controller_id_sha256,
        boot_id: request.boot_id,
        session_id: request.session_id,
        request_start_time_ns: request.request_start_time_ns,
        final_sequence,
    }
}

fn gate0_session(
    final_frame: SensorFrame,
    receipt: QuiesceReceipt,
    final_frame_host_time_ns: u64,
    receipt_host_time_ns: u64,
) -> FinalizedSession {
    let mut writer = SessionWriter::new(Vec::new(), header()).expect("header writes");
    let request = quiesce_request();
    writer
        .append_quiesce_request(
            ControllerStream::Left,
            &request,
            request.request_start_time_ns,
        )
        .expect("request writes");
    writer
        .append_frame(
            ControllerStream::Left,
            &final_frame,
            final_frame_host_time_ns,
        )
        .expect("final frame writes");
    writer
        .append_quiesce_receipt(ControllerStream::Left, &receipt, receipt_host_time_ns)
        .expect("receipt writes");
    writer.finish_finalized().expect("IKS3 finalizes")
}

fn frame(hand: Hand, sequence: u32, device_time_us: u64) -> SensorFrame {
    SensorFrame {
        hand,
        controller_id_sha256: match hand {
            Hand::Left => [0x11; 32],
            Hand::Right => [0x22; 32],
        },
        boot_id: match hand {
            Hand::Left => [0x31; 16],
            Hand::Right => [0x32; 16],
        },
        flags: 0,
        valid_sensor_mask: ALL_SENSORS_VALID,
        sequence,
        device_time_us,
        sensors: [SensorReading::default(); SENSOR_COUNT],
    }
}

fn reference_identities() -> [FrameIdentity; 2] {
    [
        FrameIdentity {
            hand: Hand::Left,
            controller_id_sha256: [0x11; 32],
            boot_id: [0x31; 16],
        },
        FrameIdentity {
            hand: Hand::Right,
            controller_id_sha256: [0x22; 32],
            boot_id: [0x32; 16],
        },
    ]
}

#[test]
fn every_candidate_retains_its_physical_controller_stream() {
    let valid = invisible_keyboard::protocol::encode_frame(&frame(Hand::Left, 1, 5_000))
        .expect("valid frame encodes");
    let mut writer = SessionWriter::new(Vec::new(), header()).expect("header writes");
    writer
        .append_candidate(
            ControllerStream::Left,
            SessionRecordKind::Decoded,
            &valid,
            u64::try_from(valid.len()).expect("length fits"),
            1,
        )
        .expect("decoded candidate writes");
    writer
        .append_candidate(
            ControllerStream::Right,
            SessionRecordKind::Malformed,
            &[4, 1, 2],
            3,
            2,
        )
        .expect("malformed candidate writes");
    writer
        .append_candidate(
            ControllerStream::Left,
            SessionRecordKind::Oversized,
            &[0x7e; 113],
            9_001,
            3,
        )
        .expect("oversized candidate writes");
    writer
        .append_candidate(
            ControllerStream::Right,
            SessionRecordKind::Unterminated,
            &[3, 1, 2],
            3,
            4,
        )
        .expect("unterminated candidate writes");

    let finalized = writer
        .finish_finalized()
        .expect("complete source-aware session finalizes");
    let mut reader = finalized.reader();
    let mut sources = Vec::new();
    while let Some(record) = reader
        .next_record()
        .expect("audited record remains readable")
    {
        sources.push(record.source);
    }

    assert_eq!(
        sources,
        [
            ControllerStream::Left,
            ControllerStream::Right,
            ControllerStream::Left,
            ControllerStream::Right,
        ]
    );
}

#[test]
fn wrong_claimed_hand_is_accounted_and_strict_finalization_fails_closed() {
    let mut writer = SessionWriter::new(Vec::new(), header()).expect("header writes");
    writer
        .append_frame(
            ControllerStream::Left,
            &frame(Hand::Right, 7, 35_000),
            70_000,
        )
        .expect("mismatched evidence is still preserved");

    let live = writer.accounting_snapshot();
    assert_eq!(live.total_records, 1);
    assert_eq!(live.invalid_records, 1);
    assert_eq!(live.source_payload_hand_mismatches, 1);
    assert_eq!(live.left.received, 0);
    assert_eq!(live.right.received, 0);

    assert!(matches!(
        writer.finish_finalized(),
        Err(SessionError::SourcePayloadHandMismatch {
            record_index: 0,
            source: ControllerStream::Left,
            claimed: Hand::Right,
        })
    ));
}

#[test]
fn rejected_provenance_keeps_footer_closed_bytes_for_incident_diagnosis() {
    let mut writer = SessionWriter::new(Vec::new(), header()).expect("header writes");
    writer
        .append_frame(
            ControllerStream::Left,
            &frame(Hand::Right, 7, 35_000),
            70_000,
        )
        .expect("mismatched source evidence is retained");

    let closed = writer.finish_closed().expect("incident stream closes");
    assert_eq!(closed.summary().total_records, 1);
    assert_eq!(closed.summary().source_payload_hand_mismatches, 1);
    assert_eq!(
        &closed.bytes()[closed.bytes().len() - FOOTER_LEN..][..4],
        b"END3"
    );
    assert!(matches!(
        closed.admit(),
        Err(SessionError::SourcePayloadHandMismatch {
            record_index: 0,
            source: ControllerStream::Left,
            claimed: Hand::Right,
        })
    ));
    assert_eq!(
        &closed.bytes()[closed.bytes().len() - FOOTER_LEN..][..4],
        b"END3"
    );
}

#[test]
fn dropping_a_partial_writer_cannot_produce_a_finalized_session() {
    let mut bytes = Vec::new();
    {
        let mut writer = SessionWriter::new(&mut bytes, header()).expect("header writes");
        writer
            .append_frame(ControllerStream::Left, &frame(Hand::Left, 1, 5_000), 10_000)
            .expect("record writes");
    }

    assert!(matches!(
        FinalizedSession::audit(bytes),
        Err(SessionError::MissingFooter { record_count: 1 })
    ));
}

#[test]
fn valid_records_round_trip_in_append_order() {
    let mut writer = SessionWriter::new(Vec::new(), header()).expect("header writes");
    writer
        .append_frame(
            ControllerStream::Left,
            &frame(Hand::Left, 10, 1_000),
            50_000,
        )
        .expect("left frame writes");
    writer
        .append_frame(
            ControllerStream::Right,
            &frame(Hand::Right, 20, 1_100),
            50_100,
        )
        .expect("right frame writes");
    let finalized = writer.finish_finalized().expect("session finishes");
    let summary = finalized.summary();

    assert_eq!(summary.total_records, 2);
    assert_eq!(summary.invalid_records, 0);
    assert_eq!(summary.left.received, 1);
    assert_eq!(summary.right.received, 1);

    let mut reader = finalized.reader();
    assert_eq!(reader.header(), &header());

    let first = reader
        .next_record()
        .expect("record read succeeds")
        .expect("first record exists");
    let second = reader
        .next_record()
        .expect("record read succeeds")
        .expect("second record exists");

    assert_eq!(first.host_time_ns, 50_000);
    assert_eq!(first.decode().expect("valid frame").sequence, 10);
    assert_eq!(second.host_time_ns, 50_100);
    assert_eq!(second.decode().expect("valid frame").sequence, 20);
    assert!(reader.next_record().expect("clean EOF").is_none());
}

#[test]
fn reference_session_round_trips_through_strict_audit() {
    let (bytes, written) = write_reference_session(
        Vec::new(),
        CORE28_MANIFEST_SHA256,
        5_000,
        reference_identities(),
    )
    .expect("fixture writes");
    let finalized = FinalizedSession::audit(bytes).expect("fixture strictly replays");

    assert_eq!(written.total_records, 64);
    assert_eq!(written.invalid_records, 0);
    assert_eq!(*finalized.summary(), written);
    assert_eq!(finalized.summary().left.received, 32);
    assert_eq!(finalized.summary().right.received, 32);
    assert_eq!(finalized.summary().left.missing, 0);
    assert_eq!(finalized.summary().right.missing, 0);
    assert_eq!(finalized.header().nominal_period_us, 5_000);
    assert_eq!(
        finalized.sha256(),
        invisible_keyboard::manifest::sha256(finalized.bytes())
    );
}

#[test]
fn raw_malformed_payload_is_preserved_and_counted() {
    let malformed = vec![0xde, 0xad, 0xbe, 0xef];
    let mut writer = SessionWriter::new(Vec::new(), header()).expect("header writes");
    writer
        .append_candidate(
            ControllerStream::Right,
            SessionRecordKind::Malformed,
            &malformed,
            u64::try_from(malformed.len()).expect("length fits"),
            99_000,
        )
        .expect("raw payload writes");
    let finalized = writer.finish_finalized().expect("session finishes");
    let summary = finalized.summary();

    assert_eq!(summary.total_records, 1);
    assert_eq!(summary.invalid_records, 1);
    assert_eq!(summary.malformed_candidates, 1);

    let mut reader = finalized.reader();
    let record = reader
        .next_record()
        .expect("record read succeeds")
        .expect("record exists");
    assert_eq!(record.kind, SessionRecordKind::Malformed);
    assert_eq!(record.original_len, 4);
    assert_eq!(record.payload, malformed);
    assert!(record.decode().is_err());
}

#[test]
fn transport_candidate_provenance_survives_strict_replay() {
    let valid = invisible_keyboard::protocol::encode_frame(&frame(Hand::Left, 1, 5_000))
        .expect("valid frame encodes");
    let malformed = [4_u8, 1, 2];
    let oversized_prefix = [0x7e_u8; 113];
    let unterminated = [3_u8, 1, 2];
    let mut writer = SessionWriter::new(Vec::new(), header()).expect("header writes");
    writer
        .append_candidate(
            ControllerStream::Left,
            SessionRecordKind::Decoded,
            &valid,
            u64::try_from(valid.len()).expect("length fits"),
            1,
        )
        .expect("decoded candidate writes");
    writer
        .append_candidate(
            ControllerStream::Right,
            SessionRecordKind::Malformed,
            &malformed,
            u64::try_from(malformed.len()).expect("length fits"),
            2,
        )
        .expect("malformed candidate writes");
    writer
        .append_candidate(
            ControllerStream::Left,
            SessionRecordKind::Oversized,
            &oversized_prefix,
            9_001,
            3,
        )
        .expect("oversized candidate writes");
    writer
        .append_candidate(
            ControllerStream::Right,
            SessionRecordKind::Unterminated,
            &unterminated,
            u64::try_from(unterminated.len()).expect("length fits"),
            4,
        )
        .expect("unterminated candidate writes");
    let (bytes, written) = writer.finish().expect("session finishes");

    assert_eq!(written.total_records, 4);
    assert_eq!(written.invalid_records, 3);
    assert_eq!(written.decoded_candidates, 1);
    assert_eq!(written.malformed_candidates, 1);
    assert_eq!(written.oversized_candidates, 1);
    assert_eq!(written.unterminated_candidates, 1);

    let finalized = FinalizedSession::audit(bytes).expect("candidate kinds replay");
    assert_eq!(*finalized.summary(), written);
    let mut reader = finalized.reader();
    assert_eq!(
        reader
            .next_record()
            .expect("decoded record reads")
            .expect("decoded record exists")
            .kind,
        SessionRecordKind::Decoded
    );
    assert_eq!(
        reader
            .next_record()
            .expect("malformed record reads")
            .expect("malformed record exists")
            .kind,
        SessionRecordKind::Malformed
    );
    let oversized = reader
        .next_record()
        .expect("oversized record reads")
        .expect("oversized record exists");
    assert_eq!(oversized.kind, SessionRecordKind::Oversized);
    assert_eq!(oversized.payload.len(), 113);
    assert_eq!(oversized.original_len, 9_001);
    assert_eq!(
        reader
            .next_record()
            .expect("unterminated record reads")
            .expect("unterminated record exists")
            .kind,
        SessionRecordKind::Unterminated
    );
}

#[test]
fn impossible_candidate_lengths_are_rejected_before_writing() {
    let mut writer = SessionWriter::new(Vec::new(), header()).expect("header writes");
    assert!(matches!(
        writer.append_candidate(
            ControllerStream::Left,
            SessionRecordKind::Malformed,
            &[1, 2, 3],
            2,
            99,
        ),
        Err(SessionError::InvalidCandidateLength { .. })
    ));
    assert!(matches!(
        writer.append_candidate(
            ControllerStream::Right,
            SessionRecordKind::Oversized,
            &[1, 2, 3],
            3,
            99,
        ),
        Err(SessionError::InvalidCandidateLength { .. })
    ));
    let (bytes, summary) = writer.finish().expect("session finishes");
    assert_eq!(bytes.len(), HEADER_LEN + FOOTER_LEN);
    assert_eq!(summary.total_records, 0);
}

#[test]
fn nondecoded_provenance_cannot_be_bypassed_by_frame_shaped_bytes() {
    let valid = invisible_keyboard::protocol::encode_frame(&frame(Hand::Left, 7, 35_000))
        .expect("valid frame encodes");
    let mut writer = SessionWriter::new(Vec::new(), header()).expect("header writes");
    writer
        .append_candidate(
            ControllerStream::Right,
            SessionRecordKind::Malformed,
            &valid,
            u64::try_from(valid.len()).expect("length fits"),
            9,
        )
        .expect("malformed candidate writes");
    let finalized = writer.finish_finalized().expect("session finishes");
    let summary = finalized.summary();
    assert_eq!(summary.invalid_records, 1);
    assert_eq!(summary.left.received, 0);
    assert_eq!(summary.right.received, 0);
    assert_eq!(summary.source_payload_hand_mismatches, 0);

    let mut reader = finalized.reader();
    let record = reader
        .next_record()
        .expect("record reads")
        .expect("record exists");
    assert!(matches!(
        record.decode(),
        Err(RecordDecodeError::CandidateNotDecoded(
            SessionRecordKind::Malformed
        ))
    ));
}

#[test]
fn sequence_anomalies_and_clock_regressions_are_visible() {
    let mut writer = SessionWriter::new(Vec::new(), header()).expect("header writes");
    let observations = [
        (10, 1_000, 50_000),
        (11, 2_000, 51_000),
        (14, 3_000, 52_000),
        (14, 4_000, 53_000),
        (13, 5_000, 54_000),
        (15, 4_500, 53_500),
    ];

    for (sequence, device_time_us, host_time_ns) in observations {
        writer
            .append_frame(
                ControllerStream::Left,
                &frame(Hand::Left, sequence, device_time_us),
                host_time_ns,
            )
            .expect("frame writes");
    }

    let (_, summary) = writer.finish().expect("session finishes");
    assert_eq!(summary.left.received, 6);
    assert_eq!(summary.left.missing, 2);
    assert_eq!(summary.left.duplicates, 1);
    assert_eq!(summary.left.reordered, 1);
    assert_eq!(summary.left.device_time_regressions, 1);
    assert_eq!(summary.left.host_time_regressions, 1);
}

#[test]
fn sequence_wrap_is_in_order() {
    let mut writer = SessionWriter::new(Vec::new(), header()).expect("header writes");
    writer
        .append_frame(
            ControllerStream::Right,
            &frame(Hand::Right, u32::MAX, 1_000),
            50_000,
        )
        .expect("frame writes");
    writer
        .append_frame(
            ControllerStream::Right,
            &frame(Hand::Right, 0, 2_000),
            51_000,
        )
        .expect("frame writes");
    let (_, summary) = writer.finish().expect("session finishes");

    assert_eq!(summary.right.received, 2);
    assert_eq!(summary.right.missing, 0);
    assert_eq!(summary.right.duplicates, 0);
    assert_eq!(summary.right.reordered, 0);
}

#[test]
fn same_inputs_produce_byte_identical_session_files() {
    fn write_once() -> Vec<u8> {
        let mut writer = SessionWriter::new(Vec::new(), header()).expect("header writes");
        writer
            .append_frame(ControllerStream::Left, &frame(Hand::Left, 1, 5_000), 10_000)
            .expect("frame writes");
        writer
            .append_frame(
                ControllerStream::Right,
                &frame(Hand::Right, 2, 5_100),
                10_100,
            )
            .expect("frame writes");
        writer.finish().expect("session finishes").0
    }

    assert_eq!(write_once(), write_once());
}

#[test]
fn record_corruption_is_distinct_from_captured_frame_corruption() {
    let mut writer = SessionWriter::new(Vec::new(), header()).expect("header writes");
    writer
        .append_frame(ControllerStream::Left, &frame(Hand::Left, 1, 5_000), 10_000)
        .expect("frame writes");
    let (mut bytes, _) = writer.finish().expect("session finishes");

    let payload_offset = HEADER_LEN + RECORD_PREFIX_LEN;
    bytes[payload_offset + FRAME_LEN / 2] ^= 1;

    assert!(matches!(
        FinalizedSession::audit(bytes),
        Err(SessionError::RecordChecksumMismatch {
            record_index: 0,
            ..
        })
    ));
}

#[test]
fn every_partial_record_is_reported_as_truncated() {
    let mut writer = SessionWriter::new(Vec::new(), header()).expect("header writes");
    writer
        .append_frame(ControllerStream::Left, &frame(Hand::Left, 1, 5_000), 10_000)
        .expect("frame writes");
    let (bytes, _) = writer.finish().expect("session finishes");

    let footer_offset = bytes.len() - FOOTER_LEN;
    for cut in HEADER_LEN + 1..footer_offset {
        assert!(matches!(
            FinalizedSession::audit(bytes[..cut].to_vec()),
            Err(SessionError::TruncatedRecord { record_index: 0 })
        ));
    }
}

#[test]
fn complete_record_boundary_without_footer_is_rejected() {
    let mut writer = SessionWriter::new(Vec::new(), header()).expect("header writes");
    writer
        .append_frame(ControllerStream::Left, &frame(Hand::Left, 1, 5_000), 10_000)
        .expect("frame writes");
    let (bytes, _) = writer.finish().expect("session finishes");
    let footer_offset = bytes.len() - FOOTER_LEN;

    assert!(matches!(
        FinalizedSession::audit(bytes[..footer_offset].to_vec()),
        Err(SessionError::MissingFooter { record_count: 1 })
    ));
}

#[test]
fn every_partial_footer_is_rejected() {
    let mut writer = SessionWriter::new(Vec::new(), header()).expect("header writes");
    writer
        .append_frame(ControllerStream::Left, &frame(Hand::Left, 1, 5_000), 10_000)
        .expect("frame writes");
    let (bytes, _) = writer.finish().expect("session finishes");
    let footer_offset = bytes.len() - FOOTER_LEN;

    for cut in footer_offset + 1..bytes.len() {
        assert!(
            matches!(
                FinalizedSession::audit(bytes[..cut].to_vec()),
                Err(SessionError::TruncatedFooter)
            ),
            "partial footer ending at byte {cut} was accepted"
        );
    }
}

#[test]
fn trailing_bytes_after_footer_are_rejected() {
    let (mut bytes, _) = write_reference_session(
        Vec::new(),
        CORE28_MANIFEST_SHA256,
        5_000,
        reference_identities(),
    )
    .expect("fixture writes");
    bytes.push(0);

    assert!(matches!(
        FinalizedSession::audit(bytes),
        Err(SessionError::TrailingBytes)
    ));
}

#[test]
fn every_single_bit_record_corruption_is_rejected() {
    let mut writer = SessionWriter::new(Vec::new(), header()).expect("header writes");
    writer
        .append_frame(ControllerStream::Left, &frame(Hand::Left, 1, 5_000), 10_000)
        .expect("frame writes");
    let (bytes, _) = writer.finish().expect("session finishes");

    for byte_index in HEADER_LEN..bytes.len() {
        for bit_index in 0..8 {
            let mut corrupted = bytes.clone();
            corrupted[byte_index] ^= 1 << bit_index;
            assert!(
                FinalizedSession::audit(corrupted).is_err(),
                "corruption at post-header byte {}, bit {bit_index} was accepted",
                byte_index - HEADER_LEN
            );
        }
    }
}

#[test]
fn acquisition_specific_session_caps_match_their_exact_physical_contracts() {
    assert_eq!(ACQUISITION_RECORDS_PER_SECOND_PER_HAND, 200);
    assert_eq!(MAX_ACQUISITION_RECORD_BYTES, 193);
    assert_eq!(MAX_SETUP_SESSION_RECORDS, 480_000);
    assert_eq!(MAX_SETUP_SESSION_BYTES, 92_640_080);
    assert_eq!(GATE0A_CAPTURE_TIME_SECONDS, 3_610);
    assert_eq!(MAX_GATE0A_SESSION_RECORDS, 1_444_002);
    assert_eq!(MAX_GATE0A_SESSION_BYTES, 278_692_316);

    assert!(validate_calibration_session_size(MAX_SETUP_SESSION_BYTES).is_ok());
    assert!(matches!(
        validate_calibration_session_size(MAX_SETUP_SESSION_BYTES + 1),
        Err(SessionError::CalibrationSessionTooLarge {
            actual,
            maximum: MAX_SETUP_SESSION_BYTES,
        }) if actual == MAX_SETUP_SESSION_BYTES + 1
    ));
    assert!(validate_gate0a_session_size(MAX_GATE0A_SESSION_BYTES).is_ok());
    assert!(matches!(
        validate_gate0a_session_size(MAX_GATE0A_SESSION_BYTES + 1),
        Err(SessionError::Gate0aSessionTooLarge {
            actual,
            maximum: MAX_GATE0A_SESSION_BYTES,
        }) if actual == MAX_GATE0A_SESSION_BYTES + 1
    ));
}

#[test]
fn streaming_audit_matches_owned_evidence_without_a_second_hash_path() {
    let (bytes, _) = write_reference_session(
        Vec::new(),
        CORE28_MANIFEST_SHA256,
        5_000,
        reference_identities(),
    )
    .expect("fixture writes");
    let finalized = FinalizedSession::audit(bytes.clone()).expect("owned evidence audits");

    let streamed = audit_session_reader(bytes.as_slice()).expect("streamed evidence audits");

    assert_eq!(streamed.sha256(), finalized.sha256());
    assert_eq!(streamed.header(), finalized.header());
    assert_eq!(streamed.summary(), finalized.summary());
    assert_eq!(
        streamed.byte_len(),
        u64::try_from(bytes.len()).expect("fixture length fits u64")
    );
}

#[test]
fn streaming_audit_rejects_corruption_at_the_end_of_the_footer() {
    let (mut bytes, _) = write_reference_session(
        Vec::new(),
        CORE28_MANIFEST_SHA256,
        5_000,
        reference_identities(),
    )
    .expect("fixture writes");
    let final_byte = bytes.last_mut().expect("fixture has a footer");
    *final_byte ^= 0x80;

    assert!(matches!(
        audit_session_reader(bytes.as_slice()),
        Err(SessionError::FooterChecksumMismatch { .. })
    ));
}

#[test]
fn iks3_magic_is_a_hard_cut_and_checksum_valid_iks2_is_rejected() {
    let mut writer = SessionWriter::new(Vec::new(), header()).expect("header writes");
    writer
        .append_frame(ControllerStream::Left, &frame(Hand::Left, 1, 5_000), 10_000)
        .expect("frame writes");
    let (bytes, _) = writer.finish().expect("session finishes");
    assert_eq!(&bytes[..4], b"IKS3");
    assert_eq!(&bytes[HEADER_LEN..HEADER_LEN + 4], b"REC3");
    assert_eq!(&bytes[bytes.len() - FOOTER_LEN..][..4], b"END3");

    let mut legacy = bytes;
    legacy[0..4].copy_from_slice(b"IKS2");
    legacy[4] = 2;
    let header_checksum = invisible_keyboard::protocol::checksum(&legacy[..HEADER_LEN - 4]);
    legacy[HEADER_LEN - 4..HEADER_LEN].copy_from_slice(&header_checksum.to_le_bytes());
    assert!(matches!(
        FinalizedSession::audit(legacy),
        Err(SessionError::BadHeaderMagic)
    ));
}

#[test]
fn flush_publishes_the_header_but_only_finish_writes_end3() {
    let mut bytes = Vec::new();
    {
        let mut writer = SessionWriter::new(&mut bytes, header()).expect("header writes");
        writer.flush().expect("header flushes");
    }
    assert_eq!(bytes.len(), HEADER_LEN);
    assert_eq!(&bytes[..4], b"IKS3");
    assert!(!bytes.windows(4).any(|window| window == b"END3"));
    assert!(matches!(
        FinalizedSession::audit(bytes),
        Err(SessionError::MissingFooter { record_count: 0 })
    ));
}

#[test]
fn control_records_round_trip_and_terminal_audit_returns_the_exact_binding() {
    let final_frame = frame(Hand::Left, 77, 385_000);
    let finalized = gate0_session(final_frame, quiesce_receipt(77), 200, 500);
    let summary = finalized.summary();
    assert_eq!(summary.total_records, 3);
    assert_eq!(summary.decoded_candidates, 1);
    assert_eq!(summary.quiesce_requests, 1);
    assert_eq!(summary.quiesce_receipts, 1);
    assert_eq!(summary.invalid_records, 0);
    assert!(!summary.is_clean_two_hand_sensor_stream());

    let mut reader = finalized.reader();
    let request_record = reader
        .next_record()
        .expect("request reads")
        .expect("request exists");
    assert_eq!(request_record.kind, SessionRecordKind::QuiesceRequest);
    assert_eq!(
        request_record
            .decode_quiesce_request()
            .expect("request decodes"),
        quiesce_request()
    );
    let final_record = reader
        .next_record()
        .expect("frame reads")
        .expect("frame exists");
    assert_eq!(final_record.decode().expect("frame decodes"), final_frame);
    let receipt_record = reader
        .next_record()
        .expect("receipt reads")
        .expect("receipt exists");
    assert_eq!(receipt_record.kind, SessionRecordKind::QuiesceReceipt);
    assert_eq!(
        receipt_record
            .decode_quiesce_receipt()
            .expect("receipt decodes"),
        quiesce_receipt(77)
    );
    assert!(reader.next_record().expect("footer reads").is_none());

    let binding = finalized
        .gate0a_terminal_binding()
        .expect("terminal binding audits");
    assert_eq!(binding.source, ControllerStream::Left);
    assert_eq!(binding.request, quiesce_request());
    assert_eq!(binding.receipt, quiesce_receipt(77));
    assert_eq!(binding.final_frame, final_frame);
    assert_eq!(binding.request_record_index, 0);
    assert_eq!(binding.final_frame_record_index, 1);
    assert_eq!(binding.receipt_record_index, 2);
    assert_eq!(binding.request_host_time_ns, 100);
    assert_eq!(binding.final_frame_host_time_ns, 200);
    assert_eq!(binding.receipt_host_time_ns, 500);
    assert_eq!(binding.response_time_ns, 400);

    let streamed = audit_gate0a_session_reader(finalized.bytes()).expect("streamed Gate 0A audits");
    assert_eq!(streamed.session().sha256(), finalized.sha256());
    assert_eq!(streamed.terminal(), &binding);
}

#[test]
fn terminal_audit_rejects_timeout_binding_mismatch_and_dirty_final_frame() {
    let final_frame = frame(Hand::Left, 77, 385_000);
    let timeout = gate0_session(
        final_frame,
        quiesce_receipt(77),
        200,
        quiesce_request().request_start_time_ns + GATE0A_HOST_RESPONSE_TIMEOUT_NS + 1,
    );
    assert!(matches!(
        timeout.gate0a_terminal_binding(),
        Err(Gate0aTerminalError::ResponseTimeout { actual_ns, .. })
            if actual_ns == GATE0A_HOST_RESPONSE_TIMEOUT_NS + 1
    ));

    let mut mismatched_receipt = quiesce_receipt(77);
    mismatched_receipt.boot_id = [0x44; 16];
    let mismatch = gate0_session(final_frame, mismatched_receipt, 200, 500);
    assert!(matches!(
        mismatch.gate0a_terminal_binding(),
        Err(Gate0aTerminalError::ControlBindingMismatch)
    ));

    let mut dirty_frame = final_frame;
    dirty_frame.flags = FLAG_CLOCK_UNSYNCED;
    let dirty = gate0_session(dirty_frame, quiesce_receipt(77), 200, 500);
    assert!(matches!(
        dirty.gate0a_terminal_binding(),
        Err(Gate0aTerminalError::FinalFrameNotClean { .. })
    ));

    let bad_time_order = gate0_session(final_frame, quiesce_receipt(77), 99, 500);
    assert!(matches!(
        bad_time_order.gate0a_terminal_binding(),
        Err(Gate0aTerminalError::TerminalHostTimeOrder {
            request_ns: 100,
            frame_ns: 99,
            receipt_ns: 500,
        })
    ));
}

#[test]
fn terminal_audit_requires_receipt_last() {
    let request = quiesce_request();
    let receipt = quiesce_receipt(77);
    let mut writer = SessionWriter::new(Vec::new(), header()).expect("header writes");
    writer
        .append_quiesce_request(ControllerStream::Left, &request, 100)
        .expect("request writes");
    writer
        .append_frame(ControllerStream::Left, &frame(Hand::Left, 77, 385_000), 200)
        .expect("final frame writes");
    writer
        .append_quiesce_receipt(ControllerStream::Left, &receipt, 500)
        .expect("receipt writes");
    writer
        .append_frame(ControllerStream::Left, &frame(Hand::Left, 78, 390_000), 600)
        .expect("trailing frame writes");
    let trailing = writer.finish_finalized().expect("container audits");
    assert!(matches!(
        trailing.gate0a_terminal_binding(),
        Err(Gate0aTerminalError::ReceiptNotLast {
            receipt_record_index: 2,
            trailing_record_index: 3,
        })
    ));
}

#[test]
fn terminal_audit_requires_a_frame_immediately_before_receipt() {
    let request = quiesce_request();
    let receipt = quiesce_receipt(77);
    let mut writer = SessionWriter::new(Vec::new(), header()).expect("header writes");
    writer
        .append_quiesce_request(ControllerStream::Left, &request, 100)
        .expect("request writes");
    writer
        .append_quiesce_receipt(ControllerStream::Left, &receipt, 500)
        .expect("receipt writes");
    let no_frame = writer.finish_finalized().expect("container audits");
    assert!(matches!(
        no_frame.gate0a_terminal_binding(),
        Err(Gate0aTerminalError::ReceiptWithoutImmediateFrame {
            receipt_record_index: 1,
        })
    ));
}

#[test]
fn terminal_audit_rejects_an_invalid_record_before_receipt() {
    let request = quiesce_request();
    let receipt = quiesce_receipt(77);
    let mut writer = SessionWriter::new(Vec::new(), header()).expect("header writes");
    writer
        .append_quiesce_request(ControllerStream::Left, &request, 100)
        .expect("request writes");
    writer
        .append_candidate(
            ControllerStream::Left,
            SessionRecordKind::Malformed,
            &[1],
            1,
            200,
        )
        .expect("malformed evidence writes");
    writer
        .append_quiesce_receipt(ControllerStream::Left, &receipt, 500)
        .expect("receipt writes");
    let no_frame = writer.finish_finalized().expect("container audits");
    assert!(matches!(
        no_frame.gate0a_terminal_binding(),
        Err(Gate0aTerminalError::InvalidRecords { actual: 1 })
    ));
}

#[test]
fn strict_session_audit_rejects_semantically_invalid_control_records() {
    let mut invalid_request =
        invisible_keyboard::protocol::encode_quiesce_request(&quiesce_request())
            .expect("request encodes");
    invalid_request[7] = 1;
    let wire_checksum = invisible_keyboard::protocol::checksum(&invalid_request[..80]);
    invalid_request[80..84].copy_from_slice(&wire_checksum.to_le_bytes());

    let mut writer = SessionWriter::new(Vec::new(), header()).expect("header writes");
    writer
        .append_candidate(
            ControllerStream::Left,
            SessionRecordKind::QuiesceRequest,
            &invalid_request,
            u64::try_from(invalid_request.len()).expect("request length fits"),
            100,
        )
        .expect("invalid control evidence remains writable");
    let (bytes, summary) = writer.finish().expect("container closes");
    assert_eq!(summary.quiesce_requests, 1);
    assert_eq!(summary.invalid_records, 1);
    assert!(matches!(
        FinalizedSession::audit(bytes),
        Err(SessionError::InvalidQuiesceRecord {
            record_index: 0,
            error: invisible_keyboard::protocol::QuiesceRecordError::NonzeroReserved(1),
        })
    ));

    let valid_request = invisible_keyboard::protocol::encode_quiesce_request(&quiesce_request())
        .expect("request encodes");
    let mut writer = SessionWriter::new(Vec::new(), header()).expect("header writes");
    assert!(matches!(
        writer.append_candidate(
            ControllerStream::Left,
            SessionRecordKind::QuiesceRequest,
            &valid_request,
            u64::try_from(valid_request.len() + 1).expect("request length fits"),
            100,
        ),
        Err(SessionError::InvalidCandidateLength {
            kind: SessionRecordKind::QuiesceRequest,
            ..
        })
    ));
}
