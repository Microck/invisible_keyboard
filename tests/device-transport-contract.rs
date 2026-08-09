use invisible_keyboard::{
    device_transport::{
        CandidateIdentity, DeviceTransport, DeviceTransportConfig,
        DeviceTransportConfigurationError, DeviceTransportError, DeviceTransportEvent,
        DeviceTransportFault, DeviceWireIdentityBootstrap, Gate0DeviceTransport,
        Gate0DeviceTransportError, Gate0DeviceTransportFault, IdentityBootstrapConfig,
        IdentityBootstrapError, ObservedQuiesceReceipt,
    },
    layout::Hand,
    protocol::{
        ALL_SENSORS_VALID, FrameIdentity, QuiesceReceipt, QuiesceRequest, SENSOR_COUNT,
        SensorFrame, SensorReading, decode_frame,
    },
    transport::{
        MAX_ENCODED_FRAME_LEN, TransportCandidate, encode_transport_frame,
        encode_transport_quiesce_receipt, encode_transport_quiesce_request,
    },
};

fn config() -> DeviceTransportConfig {
    DeviceTransportConfig {
        merge_lateness_ns: 10,
        source_silence_timeout_ns: 50,
        max_pending_candidates: 16,
    }
}

fn frame(hand: Hand, sequence: u32) -> SensorFrame {
    let identity = identity(hand);
    SensorFrame {
        hand,
        controller_id_sha256: identity.controller_id_sha256,
        boot_id: identity.boot_id,
        flags: 0,
        valid_sensor_mask: ALL_SENSORS_VALID,
        sequence,
        device_time_us: u64::from(sequence) * 5_000,
        sensors: [SensorReading::default(); SENSOR_COUNT],
    }
}

fn identity(hand: Hand) -> FrameIdentity {
    FrameIdentity {
        hand,
        controller_id_sha256: match hand {
            Hand::Left => [0x11; 32],
            Hand::Right => [0x22; 32],
        },
        boot_id: match hand {
            Hand::Left => [0x31; 16],
            Hand::Right => [0x32; 16],
        },
    }
}

fn encoded(frame: &SensorFrame) -> Vec<u8> {
    encode_transport_frame(frame).expect("valid frame encodes")
}

fn bootstrap_config() -> IdentityBootstrapConfig {
    IdentityBootstrapConfig {
        timeout_ns: 100,
        max_buffered_bytes: 4_096,
        max_buffered_events: 16,
    }
}

fn request(hand: Hand) -> QuiesceRequest {
    let identity = identity(hand);
    QuiesceRequest {
        hand,
        controller_id_sha256: identity.controller_id_sha256,
        boot_id: identity.boot_id,
        session_id: [0x77; 16],
        request_start_time_ns: 123_456,
    }
}

fn receipt(hand: Hand, final_sequence: u32) -> QuiesceReceipt {
    let request = request(hand);
    QuiesceReceipt {
        hand,
        controller_id_sha256: request.controller_id_sha256,
        boot_id: request.boot_id,
        session_id: request.session_id,
        request_start_time_ns: request.request_start_time_ns,
        final_sequence,
    }
}

#[test]
fn bootstrap_discovers_both_boots_and_preserves_every_input_byte() {
    let mut bootstrap = DeviceWireIdentityBootstrap::new(
        [
            (Hand::Left, identity(Hand::Left).controller_id_sha256),
            (Hand::Right, identity(Hand::Right).controller_id_sha256),
        ],
        bootstrap_config(),
    )
    .expect("bootstrap configuration is valid");
    let left = encoded(&frame(Hand::Left, 1));
    let right = encoded(&frame(Hand::Right, 2));

    bootstrap
        .handle(bytes(Hand::Left, 10, left[..17].to_vec()))
        .expect("left prefix is buffered");
    assert!(!bootstrap.is_complete());
    bootstrap
        .handle(bytes(Hand::Right, 11, right.clone()))
        .expect("right identity is observed");
    bootstrap
        .handle(bytes(Hand::Left, 12, left[17..].to_vec()))
        .expect("left identity is observed");
    assert!(bootstrap.is_complete());

    let locked = bootstrap.finish().expect("complete bootstrap locks");
    assert_eq!(
        locked.identities,
        vec![identity(Hand::Left), identity(Hand::Right)]
    );
    assert_eq!(locked.events.len(), 3);
    assert_eq!(
        locked
            .events
            .iter()
            .filter_map(|event| match event {
                DeviceTransportEvent::Bytes { bytes, .. } => Some(bytes.len()),
                _ => None,
            })
            .sum::<usize>(),
        left.len() + right.len()
    );
}

#[test]
fn bootstrap_admits_one_declared_hand_and_returns_all_identities_in_hand_order() {
    for hand in [Hand::Left, Hand::Right] {
        let mut bootstrap = DeviceWireIdentityBootstrap::new(
            [(hand, identity(hand).controller_id_sha256)],
            bootstrap_config(),
        )
        .expect("one declared controller is valid");
        let input = encoded(&frame(hand, 7));

        bootstrap
            .handle(bytes(hand, 1, input.clone()))
            .expect("declared identity is observed");
        let locked = bootstrap.finish().expect("one-hand bootstrap locks");

        assert_eq!(locked.identities, vec![identity(hand)]);
        assert_eq!(
            locked.events,
            vec![bytes(hand, 1, input)],
            "the identity-bearing bytes remain available for one replay"
        );
    }

    let mut reversed = DeviceWireIdentityBootstrap::new(
        [
            (Hand::Right, identity(Hand::Right).controller_id_sha256),
            (Hand::Left, identity(Hand::Left).controller_id_sha256),
        ],
        bootstrap_config(),
    )
    .expect("two unique controllers are valid");
    reversed
        .handle(bytes(Hand::Right, 1, encoded(&frame(Hand::Right, 0))))
        .expect("right identity is observed");
    reversed
        .handle(bytes(Hand::Left, 2, encoded(&frame(Hand::Left, 0))))
        .expect("left identity is observed");
    assert_eq!(
        reversed
            .finish()
            .expect("two-hand bootstrap locks")
            .identities,
        vec![identity(Hand::Left), identity(Hand::Right)]
    );
}

#[test]
fn bootstrap_rejects_empty_and_duplicate_topologies() {
    assert!(matches!(
        DeviceWireIdentityBootstrap::new(
            std::iter::empty::<(Hand, [u8; 32])>(),
            bootstrap_config(),
        ),
        Err(IdentityBootstrapError::InvalidTopology)
    ));
    assert!(matches!(
        DeviceWireIdentityBootstrap::new(
            [
                (Hand::Left, identity(Hand::Left).controller_id_sha256),
                (Hand::Left, identity(Hand::Left).controller_id_sha256),
            ],
            bootstrap_config(),
        ),
        Err(IdentityBootstrapError::InvalidTopology)
    ));
}

#[test]
fn bootstrap_rejects_wrong_controller_boot_change_timeout_and_unbounded_input() {
    let expected = [
        (Hand::Left, identity(Hand::Left).controller_id_sha256),
        (Hand::Right, identity(Hand::Right).controller_id_sha256),
    ];
    let mut wrong_controller = DeviceWireIdentityBootstrap::new(expected, bootstrap_config())
        .expect("configuration is valid");
    let mut impostor = frame(Hand::Left, 1);
    impostor.controller_id_sha256 = [0x77; 32];
    assert!(matches!(
        wrong_controller.handle(bytes(Hand::Left, 1, encoded(&impostor))),
        Err(IdentityBootstrapError::ControllerIdentityMismatch(
            Hand::Left
        ))
    ));

    let mut boot_change = DeviceWireIdentityBootstrap::new(expected, bootstrap_config())
        .expect("configuration is valid");
    boot_change
        .handle(bytes(Hand::Left, 1, encoded(&frame(Hand::Left, 1))))
        .expect("first boot binds");
    let mut rebooted = frame(Hand::Left, 2);
    rebooted.boot_id = [0x55; 16];
    assert!(matches!(
        boot_change.handle(bytes(Hand::Left, 2, encoded(&rebooted))),
        Err(IdentityBootstrapError::BootIdentityChanged(Hand::Left))
    ));

    let mut timeout = DeviceWireIdentityBootstrap::new(expected, bootstrap_config())
        .expect("configuration is valid");
    timeout
        .handle(DeviceTransportEvent::Tick { host_time_ns: 1 })
        .expect("bootstrap timer starts");
    assert!(matches!(
        timeout.handle(DeviceTransportEvent::Tick { host_time_ns: 102 }),
        Err(IdentityBootstrapError::Timeout)
    ));

    let mut bounded = bootstrap_config();
    bounded.max_buffered_bytes = 8;
    let mut overflow =
        DeviceWireIdentityBootstrap::new(expected, bounded).expect("configuration is valid");
    assert!(matches!(
        overflow.handle(bytes(Hand::Left, 1, vec![1; 9])),
        Err(IdentityBootstrapError::BufferCapacityExceeded)
    ));
}

#[test]
fn gate0_dispatch_accepts_one_bound_receipt_after_its_immediate_clean_frame() {
    let hand = Hand::Left;
    let request = request(hand);
    let final_frame = frame(hand, 91);
    let receipt = receipt(hand, final_frame.sequence);
    let mut terminal = encoded(&final_frame);
    terminal.extend(
        encode_transport_quiesce_receipt(&receipt).expect("valid receipt transport encodes"),
    );

    for split in 0..=terminal.len() {
        let mut transport =
            Gate0DeviceTransport::new(identity(hand)).expect("Gate 0A identity is valid");
        transport
            .begin_request(request)
            .expect("one bound request starts");
        let mut candidates = Vec::new();
        let mut observed = None;
        for (time, chunk) in [(200, &terminal[..split]), (201, &terminal[split..])] {
            let update = transport.push(time, chunk).expect("chunk dispatches");
            assert_eq!(update.fault, None, "split {split} must remain valid");
            candidates.extend(update.candidates);
            if update.receipt.is_some() {
                assert!(observed.is_none());
                observed = update.receipt;
            }
        }
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidate_sequence(&candidates[0].candidate), Some(91));
        assert_eq!(
            observed,
            Some(ObservedQuiesceReceipt {
                receipt,
                observation_time_ns: if split == terminal.len() { 200 } else { 201 },
            })
        );
    }
}

#[test]
fn gate0_dispatch_rejects_every_byte_after_receipt_including_the_same_read() {
    let hand = Hand::Right;
    let request = request(hand);
    let final_frame = frame(hand, 42);
    let receipt = receipt(hand, 42);
    let mut transport =
        Gate0DeviceTransport::new(identity(hand)).expect("Gate 0A identity is valid");
    transport
        .begin_request(request)
        .expect("one bound request starts");

    let mut same_read = encoded(&final_frame);
    same_read.extend(
        encode_transport_quiesce_receipt(&receipt).expect("valid receipt transport encodes"),
    );
    same_read.extend([0x33, 0x44]);
    let update = transport.push(500, &same_read).expect("chunk dispatches");
    assert_eq!(update.candidates.len(), 1);
    assert_eq!(
        update.receipt.map(|observed| observed.receipt),
        Some(receipt)
    );
    assert_eq!(
        update.fault,
        Some(Gate0DeviceTransportFault::PostReceiptBytes { count: 2 })
    );

    let update = transport
        .push(501, &[0])
        .expect("later byte produces terminal evidence fault");
    assert_eq!(
        update.fault,
        Some(Gate0DeviceTransportFault::PostReceiptBytes { count: 1 })
    );
}

#[test]
fn gate0_dispatch_requires_request_binding_and_a_post_request_clean_frame() {
    let hand = Hand::Left;
    let receipt = receipt(hand, 7);
    let receipt_bytes =
        encode_transport_quiesce_receipt(&receipt).expect("valid receipt transport encodes");

    let mut before_request =
        Gate0DeviceTransport::new(identity(hand)).expect("Gate 0A identity is valid");
    let update = before_request
        .push(10, &receipt_bytes)
        .expect("receipt dispatches as retained evidence");
    assert_eq!(
        update.fault,
        Some(Gate0DeviceTransportFault::ReceiptBeforeRequest)
    );
    assert_eq!(
        update.receipt.map(|observed| observed.receipt),
        Some(receipt)
    );

    let mut no_final_frame =
        Gate0DeviceTransport::new(identity(hand)).expect("Gate 0A identity is valid");
    no_final_frame
        .push(1, &encoded(&frame(hand, 7)))
        .expect("bootstrap frame dispatches");
    no_final_frame
        .begin_request(request(hand))
        .expect("request clears pre-request adjacency");
    let update = no_final_frame
        .push(2, &receipt_bytes)
        .expect("receipt dispatches");
    assert_eq!(
        update.fault,
        Some(Gate0DeviceTransportFault::ReceiptWithoutImmediateCleanFrame)
    );

    let mut mismatched = receipt;
    mismatched.session_id = [0x88; 16];
    let mut wrong_binding =
        Gate0DeviceTransport::new(identity(hand)).expect("Gate 0A identity is valid");
    wrong_binding
        .begin_request(request(hand))
        .expect("request starts");
    let mut input = encoded(&frame(hand, 7));
    input.extend(
        encode_transport_quiesce_receipt(&mismatched)
            .expect("structurally valid mismatched receipt encodes"),
    );
    let update = wrong_binding.push(3, &input).expect("input dispatches");
    assert_eq!(
        update.fault,
        Some(Gate0DeviceTransportFault::ReceiptBindingMismatch)
    );
}

#[test]
fn gate0_dispatch_rejects_duplicate_requests_wrong_identity_and_time_regression() {
    let hand = Hand::Right;
    let mut transport =
        Gate0DeviceTransport::new(identity(hand)).expect("Gate 0A identity is valid");
    transport
        .begin_request(request(hand))
        .expect("first request starts");
    assert_eq!(
        transport.begin_request(request(hand)),
        Err(Gate0DeviceTransportError::RequestAlreadyStarted)
    );

    let outbound =
        encode_transport_quiesce_request(&request(hand)).expect("valid request transport encodes");
    let update = transport
        .push(10, &outbound)
        .expect("wrong-direction request dispatches");
    assert!(matches!(
        update.fault,
        Some(Gate0DeviceTransportFault::InvalidControllerRecord(_))
    ));
    assert!(matches!(
        transport.push(9, &[]),
        Err(Gate0DeviceTransportError::ReceiptTimeRegression {
            previous: 10,
            actual: 9,
        })
    ));

    let mut wrong_request = request(hand);
    wrong_request.boot_id = [0x99; 16];
    let mut wrong_identity =
        Gate0DeviceTransport::new(identity(hand)).expect("Gate 0A identity is valid");
    assert_eq!(
        wrong_identity.begin_request(wrong_request),
        Err(Gate0DeviceTransportError::RequestIdentityMismatch)
    );
}

fn bytes(source: Hand, receipt_time_ns: u64, bytes: impl Into<Vec<u8>>) -> DeviceTransportEvent {
    DeviceTransportEvent::Bytes {
        source,
        receipt_time_ns,
        bytes: bytes.into(),
    }
}

fn candidate_sequence(candidate: &TransportCandidate) -> Option<u32> {
    let TransportCandidate::Decoded(payload) = candidate else {
        return None;
    };
    decode_frame(payload).ok().map(|frame| frame.sequence)
}

#[test]
fn source_configuration_is_unique_and_the_input_event_is_thread_friendly() {
    fn assert_send<T: Send>() {}
    assert_send::<DeviceTransportEvent>();

    assert!(matches!(
        DeviceTransport::new(std::iter::empty::<FrameIdentity>(), config()),
        Err(DeviceTransportConfigurationError::NoSources)
    ));
    assert!(matches!(
        DeviceTransport::new([identity(Hand::Left), identity(Hand::Left)], config()),
        Err(DeviceTransportConfigurationError::DuplicateSource(
            Hand::Left
        ))
    ));
    let mut zero_capacity = config();
    zero_capacity.max_pending_candidates = 0;
    assert!(matches!(
        DeviceTransport::new([identity(Hand::Left), identity(Hand::Right)], zero_capacity),
        Err(DeviceTransportConfigurationError::ZeroPendingCapacity)
    ));
}

#[test]
fn adversarial_chunking_keeps_each_source_cobs_state_isolated() {
    let left = encoded(&frame(Hand::Left, 1));
    let right = encoded(&frame(Hand::Right, 2));
    let mut transport =
        DeviceTransport::new([identity(Hand::Left), identity(Hand::Right)], config())
            .expect("configuration is valid");

    for index in 0..left.len().max(right.len()) {
        if let Some(byte) = right.get(index) {
            let update = transport
                .handle(&bytes(Hand::Right, 100 + index as u64, [*byte]))
                .expect("right byte is accepted");
            assert!(update.candidates.is_empty());
        }
        if let Some(byte) = left.get(index) {
            let update = transport
                .handle(&bytes(Hand::Left, 100 + index as u64, [*byte]))
                .expect("left byte is accepted");
            assert!(update.candidates.is_empty());
        }
    }

    let update = transport
        .handle(&DeviceTransportEvent::Tick { host_time_ns: 500 })
        .expect("watermark advances");
    assert_eq!(
        update
            .candidates
            .iter()
            .map(|candidate| (
                candidate.source,
                candidate_sequence(&candidate.candidate),
                candidate.identity,
            ))
            .collect::<Vec<_>>(),
        [
            (Hand::Left, Some(1), CandidateIdentity::Matched),
            (Hand::Right, Some(2), CandidateIdentity::Matched),
        ]
    );
}

#[test]
fn simultaneous_frames_have_a_deterministic_source_tie_break() {
    let mut transport =
        DeviceTransport::new([identity(Hand::Right), identity(Hand::Left)], config())
            .expect("configuration is valid");

    transport
        .handle(&bytes(Hand::Right, 100, encoded(&frame(Hand::Right, 2))))
        .expect("right frame is buffered");
    transport
        .handle(&bytes(Hand::Left, 100, encoded(&frame(Hand::Left, 1))))
        .expect("left frame is buffered");

    let update = transport
        .handle(&DeviceTransportEvent::Tick { host_time_ns: 111 })
        .expect("watermark advances");
    assert_eq!(
        update
            .candidates
            .iter()
            .map(|candidate| (candidate.source, candidate_sequence(&candidate.candidate)))
            .collect::<Vec<_>>(),
        [(Hand::Left, Some(1)), (Hand::Right, Some(2))]
    );
}

#[test]
fn cross_thread_delivery_reorder_is_merged_by_receipt_time() {
    let mut transport =
        DeviceTransport::new([identity(Hand::Left), identity(Hand::Right)], config())
            .expect("configuration is valid");

    transport
        .handle(&bytes(Hand::Left, 200, encoded(&frame(Hand::Left, 20))))
        .expect("newer left delivery is buffered");
    transport
        .handle(&bytes(Hand::Right, 195, encoded(&frame(Hand::Right, 19))))
        .expect("slightly late right delivery is buffered");

    let update = transport
        .handle(&DeviceTransportEvent::Tick { host_time_ns: 211 })
        .expect("watermark advances");
    assert_eq!(
        update
            .candidates
            .iter()
            .map(|candidate| (candidate.receipt_time_ns, candidate.source))
            .collect::<Vec<_>>(),
        [(195, Hand::Right), (200, Hand::Left)]
    );
}

#[test]
fn default_merger_budgets_50_ms_for_reader_thread_delivery_reorder() {
    let mut accepted = DeviceTransport::new(
        [identity(Hand::Left), identity(Hand::Right)],
        DeviceTransportConfig::default(),
    )
    .expect("default configuration is valid");
    accepted
        .handle(&bytes(
            Hand::Left,
            100_000_000,
            encoded(&frame(Hand::Left, 1)),
        ))
        .expect("newer left receipt is buffered");
    accepted
        .handle(&bytes(
            Hand::Right,
            70_000_000,
            encoded(&frame(Hand::Right, 1)),
        ))
        .expect("a right reader delayed by 30 ms remains within budget");

    let mut rejected = DeviceTransport::new(
        [identity(Hand::Left), identity(Hand::Right)],
        DeviceTransportConfig::default(),
    )
    .expect("default configuration is valid");
    rejected
        .handle(&bytes(
            Hand::Left,
            100_000_000,
            encoded(&frame(Hand::Left, 1)),
        ))
        .expect("newer left receipt is buffered");
    assert!(matches!(
        rejected.handle(&bytes(
            Hand::Right,
            49_999_999,
            encoded(&frame(Hand::Right, 1)),
        )),
        Err(DeviceTransportError::MergeLatenessExceeded {
            source: Hand::Right,
            receipt_time_ns: 49_999_999,
            watermark_ns: 50_000_000,
        })
    ));
}

#[test]
fn wrong_hand_frames_are_retained_but_marked_as_identity_faults() {
    let mut transport =
        DeviceTransport::new([identity(Hand::Left), identity(Hand::Right)], config())
            .expect("configuration is valid");

    let update = transport
        .handle(&bytes(Hand::Left, 100, encoded(&frame(Hand::Right, 7))))
        .expect("candidate is retained as evidence");
    assert_eq!(
        update.faults,
        [DeviceTransportFault::IdentityMismatch {
            source: Hand::Left,
            expected: identity(Hand::Left),
            actual: identity(Hand::Right),
            receipt_time_ns: 100,
            source_ordinal: 0,
        }]
    );

    let update = transport
        .handle(&DeviceTransportEvent::Tick { host_time_ns: 111 })
        .expect("watermark advances");
    assert_eq!(update.candidates.len(), 1);
    assert_eq!(update.candidates[0].source, Hand::Left);
    assert_eq!(
        update.candidates[0].identity,
        CandidateIdentity::Mismatch {
            expected: identity(Hand::Left),
            actual: identity(Hand::Right),
        }
    );
    assert_eq!(candidate_sequence(&update.candidates[0].candidate), Some(7));
}

#[test]
fn one_stream_disconnect_retains_its_partial_record_and_does_not_stop_the_other() {
    let left = encoded(&frame(Hand::Left, 3));
    let mut transport =
        DeviceTransport::new([identity(Hand::Left), identity(Hand::Right)], config())
            .expect("configuration is valid");
    transport
        .handle(&bytes(Hand::Left, 100, left[..left.len() - 1].to_vec()))
        .expect("left partial candidate is buffered in its decoder");

    let update = transport
        .handle(&DeviceTransportEvent::Disconnected {
            source: Hand::Left,
            receipt_time_ns: 105,
        })
        .expect("left disconnect is observed");
    assert_eq!(
        update.faults,
        [DeviceTransportFault::SourceDisconnected {
            source: Hand::Left,
            receipt_time_ns: 105,
        }]
    );
    transport
        .handle(&bytes(Hand::Right, 106, encoded(&frame(Hand::Right, 4))))
        .expect("right source remains usable");

    let update = transport
        .handle(&DeviceTransportEvent::Tick { host_time_ns: 117 })
        .expect("watermark advances");
    assert!(matches!(
        &update.candidates[0].candidate,
        TransportCandidate::Unterminated { encoded } if !encoded.is_empty()
    ));
    assert_eq!(update.candidates[0].source, Hand::Left);
    assert_eq!(candidate_sequence(&update.candidates[1].candidate), Some(4));
    assert!(matches!(
        transport.handle(&bytes(Hand::Left, 118, [1, 0])),
        Err(DeviceTransportError::SourceDisconnected(Hand::Left))
    ));
}

#[test]
fn malformed_candidate_on_one_source_does_not_poison_the_other_source() {
    let mut transport =
        DeviceTransport::new([identity(Hand::Left), identity(Hand::Right)], config())
            .expect("configuration is valid");
    transport
        .handle(&bytes(Hand::Left, 100, [5, 1, 2, 0]))
        .expect("malformed left candidate is retained");
    transport
        .handle(&bytes(Hand::Right, 101, encoded(&frame(Hand::Right, 8))))
        .expect("right frame is decoded independently");
    transport
        .handle(&bytes(Hand::Left, 102, encoded(&frame(Hand::Left, 9))))
        .expect("left decoder recovers at its own delimiter");

    let update = transport
        .handle(&DeviceTransportEvent::Tick { host_time_ns: 113 })
        .expect("watermark advances");
    assert!(matches!(
        &update.candidates[0].candidate,
        TransportCandidate::Malformed { encoded } if encoded == &[5, 1, 2]
    ));
    assert_eq!(update.candidates[0].source, Hand::Left);
    assert_eq!(candidate_sequence(&update.candidates[1].candidate), Some(8));
    assert_eq!(candidate_sequence(&update.candidates[2].candidate), Some(9));
}

#[test]
fn oversized_candidate_retains_its_source_and_bounded_prefix() {
    let mut transport =
        DeviceTransport::new([identity(Hand::Left), identity(Hand::Right)], config())
            .expect("configuration is valid");
    let mut oversized = vec![1; MAX_ENCODED_FRAME_LEN + 7];
    oversized.push(0);
    transport
        .handle(&bytes(Hand::Right, 100, oversized))
        .expect("oversized evidence is retained");

    let update = transport
        .handle(&DeviceTransportEvent::Tick { host_time_ns: 111 })
        .expect("watermark advances");
    assert_eq!(update.candidates[0].source, Hand::Right);
    assert_eq!(
        update.candidates[0].identity,
        CandidateIdentity::Unavailable
    );
    assert!(matches!(
        &update.candidates[0].candidate,
        TransportCandidate::Oversized { prefix, actual_len }
            if prefix.len() == MAX_ENCODED_FRAME_LEN
                && *actual_len == MAX_ENCODED_FRAME_LEN + 7
    ));
}

#[test]
fn receipt_regression_late_delivery_and_source_silence_are_explicit() {
    let mut transport =
        DeviceTransport::new([identity(Hand::Left), identity(Hand::Right)], config())
            .expect("configuration is valid");
    transport
        .handle(&bytes(Hand::Left, 100, [1]))
        .expect("first left receipt is accepted");
    assert!(matches!(
        transport.handle(&bytes(Hand::Left, 99, [1])),
        Err(DeviceTransportError::ReceiptRegression {
            source: Hand::Left,
            previous: 100,
            actual: 99,
        })
    ));

    let update = transport
        .handle(&DeviceTransportEvent::Tick { host_time_ns: 151 })
        .expect("silence timeout is checked");
    assert_eq!(
        update.faults,
        [
            DeviceTransportFault::SourceSilent {
                source: Hand::Left,
                detected_time_ns: 151,
                last_receipt_time_ns: Some(100),
            },
            DeviceTransportFault::SourceSilent {
                source: Hand::Right,
                detected_time_ns: 151,
                last_receipt_time_ns: None,
            },
        ]
    );
    assert!(matches!(
        transport.handle(&bytes(Hand::Right, 140, [1])),
        Err(DeviceTransportError::MergeLatenessExceeded {
            source: Hand::Right,
            receipt_time_ns: 140,
            watermark_ns: 141,
        })
    ));
}

#[test]
fn pending_candidate_capacity_is_hard_and_overflow_is_atomic() {
    let mut bounded = config();
    bounded.max_pending_candidates = 2;
    let mut transport =
        DeviceTransport::new([identity(Hand::Left), identity(Hand::Right)], bounded)
            .expect("configuration is valid");
    let mut three = Vec::new();
    for sequence in 1..=3 {
        three.extend(encoded(&frame(Hand::Left, sequence)));
    }

    assert!(matches!(
        transport.handle(&bytes(Hand::Left, 100, three)),
        Err(DeviceTransportError::PendingCapacityExceeded {
            maximum: 2,
            required: 3,
        })
    ));
    assert_eq!(transport.pending_candidates(), 0);

    let mut two = encoded(&frame(Hand::Left, 4));
    two.extend(encoded(&frame(Hand::Left, 5)));
    transport
        .handle(&bytes(Hand::Left, 100, two))
        .expect("a chunk at the exact capacity is accepted");
    assert_eq!(transport.pending_candidates(), 2);
    let update = transport
        .handle(&DeviceTransportEvent::Tick { host_time_ns: 111 })
        .expect("watermark releases the bounded backlog");
    assert_eq!(
        update
            .candidates
            .iter()
            .filter_map(|candidate| candidate_sequence(&candidate.candidate))
            .collect::<Vec<_>>(),
        [4, 5]
    );
    assert_eq!(transport.pending_candidates(), 0);
}
