use std::{cell::RefCell, collections::VecDeque, io};

use invisible_keyboard::{
    artifact::KeyboardLayoutConfirmationReceipt,
    clock::{ClockFit, ClockSample},
    labels::{
        KeyboardEventSource, KeyboardIdentificationState, KeyboardLabelFilter,
        KeyboardSourceIdentity, LEFT_SHIFT_PHYSICAL_CODE, LabelAligner, LabelDisposition,
        LabelRejection, MonotonicHostClock, RawKeyState, RawKeyboardEvent, SensorImpulse,
    },
    layout::{Hand, all_slots},
};

fn position(label: &str) -> invisible_keyboard::layout::PhysicalPosition {
    all_slots()
        .iter()
        .find(|slot| slot.label == label)
        .expect("fixture label exists")
        .position
}

fn event(
    device_id: &str,
    code: u16,
    label: Option<&str>,
    state: RawKeyState,
    host_time_ns: u64,
) -> RawKeyboardEvent {
    RawKeyboardEvent {
        device_id: device_id.to_owned(),
        physical_code: code,
        position: label.map(position),
        modifier: false,
        state,
        kernel_time_ns: Some(host_time_ns + 9_000_000_000),
        host_time_ns,
    }
}

fn modifier_event(
    device_id: &str,
    code: u16,
    state: RawKeyState,
    host_time_ns: u64,
) -> RawKeyboardEvent {
    let mut event = event(device_id, code, None, state, host_time_ns);
    event.modifier = true;
    event
}

fn identify(filter: &mut KeyboardLabelFilter) {
    assert_eq!(
        filter.process(modifier_event(
            "keyboard-a",
            LEFT_SHIFT_PHYSICAL_CODE,
            RawKeyState::Down,
            1,
        )),
        LabelDisposition::Modifier
    );
    assert_eq!(
        filter.process(modifier_event(
            "keyboard-a",
            LEFT_SHIFT_PHYSICAL_CODE,
            RawKeyState::Up,
            2,
        )),
        LabelDisposition::Modifier
    );
    assert_eq!(
        filter.identification_state(),
        KeyboardIdentificationState::Identified
    );
}

fn source_identity() -> KeyboardSourceIdentity {
    KeyboardSourceIdentity {
        device_id: "keyboard-a".to_owned(),
        name: "Fixture Keyboard".to_owned(),
        physical_path: "usb-0000:00:14.0-1/input0".to_owned(),
        vendor_id: 0x1234,
        product_id: 0x5678,
    }
}

struct FakeMonotonicClock {
    times_ns: RefCell<VecDeque<u64>>,
}

impl FakeMonotonicClock {
    fn new(times_ns: impl IntoIterator<Item = u64>) -> Self {
        Self {
            times_ns: RefCell::new(times_ns.into_iter().collect()),
        }
    }
}

impl MonotonicHostClock for FakeMonotonicClock {
    fn now_ns(&self) -> u64 {
        self.times_ns
            .borrow_mut()
            .pop_front()
            .expect("fake clock has a timestamp for every event")
    }
}

struct FakeKeyboardEventSource {
    identity: KeyboardSourceIdentity,
    events: VecDeque<RawKeyboardEvent>,
    identity_after_next_read: Option<KeyboardSourceIdentity>,
}

impl FakeKeyboardEventSource {
    fn new(events: impl IntoIterator<Item = RawKeyboardEvent>) -> Self {
        Self {
            identity: source_identity(),
            events: events.into_iter().collect(),
            identity_after_next_read: None,
        }
    }
}

impl KeyboardEventSource for FakeKeyboardEventSource {
    fn identity(&self) -> KeyboardSourceIdentity {
        self.identity.clone()
    }

    fn read_event(&mut self, host_clock: &dyn MonotonicHostClock) -> io::Result<RawKeyboardEvent> {
        let mut event = self.events.pop_front().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "fake keyboard source is empty",
            )
        })?;
        event.host_time_ns = host_clock.now_ns();
        if let Some(identity) = self.identity_after_next_read.take() {
            self.identity = identity;
        }
        Ok(event)
    }
}

fn fit(boot: u8, host_offset_ns: u64) -> ClockFit {
    let samples = (0..20_u64)
        .map(|index| ClockSample {
            device_time_us: index * 10_000,
            host_time_ns: host_offset_ns + index * 10_000_000,
        })
        .collect::<Vec<_>>();
    ClockFit::fit([boot; 16], &samples).expect("fixture clock fits")
}

#[test]
fn selected_keyboard_filter_preserves_down_up_and_rejection_reasons() {
    let mut filter = KeyboardLabelFilter::new("keyboard-a");
    identify(&mut filter);

    let accepted = filter.process(event("keyboard-a", 30, Some("A"), RawKeyState::Down, 10));
    assert!(
        matches!(accepted, LabelDisposition::Accepted(label) if label.position == position("A"))
    );

    assert_eq!(
        filter.process(event("keyboard-a", 30, Some("A"), RawKeyState::Repeat, 11)),
        LabelDisposition::Rejected(LabelRejection::Repeat)
    );
    assert_eq!(
        filter.process(event("keyboard-a", 32, Some("D"), RawKeyState::Down, 12)),
        LabelDisposition::Rejected(LabelRejection::OverlappingPress)
    );
    assert_eq!(
        filter.process(event("keyboard-a", 32, Some("D"), RawKeyState::Up, 13)),
        LabelDisposition::Rejected(LabelRejection::OverlappingPress)
    );
    assert_eq!(
        filter.process(event("keyboard-a", 30, Some("A"), RawKeyState::Up, 14)),
        LabelDisposition::Rejected(LabelRejection::OverlappingPress)
    );
    assert!(matches!(
        filter.process(event("keyboard-a", 30, Some("A"), RawKeyState::Down, 15)),
        LabelDisposition::Accepted(_)
    ));
    assert!(matches!(
        filter.process(event("keyboard-a", 30, Some("A"), RawKeyState::Up, 16)),
        LabelDisposition::Accepted(_)
    ));

    assert_eq!(
        filter.process(event("keyboard-b", 30, Some("A"), RawKeyState::Down, 17)),
        LabelDisposition::Rejected(LabelRejection::WrongKeyboard)
    );
    assert_eq!(
        filter.process(event("keyboard-a", 99, None, RawKeyState::Down, 18)),
        LabelDisposition::Rejected(LabelRejection::UnsupportedKey)
    );
}

#[test]
fn modifiers_do_not_create_labels_or_overlap() {
    let mut filter = KeyboardLabelFilter::new("keyboard-a");
    identify(&mut filter);
    assert_eq!(
        filter.process(modifier_event("keyboard-a", 29, RawKeyState::Down, 3)),
        LabelDisposition::Modifier
    );
    assert_eq!(
        filter.process(event("keyboard-a", 30, Some("A"), RawKeyState::Down, 4)),
        LabelDisposition::Rejected(LabelRejection::ModifierCombination)
    );
    assert_eq!(
        filter.process(event("keyboard-a", 30, Some("A"), RawKeyState::Up, 5)),
        LabelDisposition::Rejected(LabelRejection::ModifierCombination)
    );
    assert_eq!(
        filter.process(modifier_event("keyboard-a", 29, RawKeyState::Repeat, 6)),
        LabelDisposition::Rejected(LabelRejection::Repeat)
    );
    assert_eq!(
        filter.process(modifier_event("keyboard-a", 29, RawKeyState::Up, 7)),
        LabelDisposition::Modifier
    );
    assert!(matches!(
        filter.process(event("keyboard-a", 30, Some("A"), RawKeyState::Down, 8)),
        LabelDisposition::Accepted(_)
    ));
}

#[test]
fn only_a_clean_selected_left_shift_round_trip_identifies_the_keyboard() {
    let mut filter = KeyboardLabelFilter::new("keyboard-a");
    assert_eq!(
        filter.process(event("keyboard-a", 30, Some("A"), RawKeyState::Down, 1)),
        LabelDisposition::Rejected(LabelRejection::KeyboardNotIdentified)
    );
    assert_eq!(
        filter.process(event("keyboard-a", 30, Some("A"), RawKeyState::Up, 2)),
        LabelDisposition::Rejected(LabelRejection::KeyboardNotIdentified)
    );

    for state in [RawKeyState::Down, RawKeyState::Up] {
        assert_eq!(
            filter.process(modifier_event(
                "keyboard-b",
                LEFT_SHIFT_PHYSICAL_CODE,
                state,
                3,
            )),
            LabelDisposition::Rejected(LabelRejection::WrongKeyboard)
        );
    }
    assert_eq!(
        filter.process(modifier_event("keyboard-a", 54, RawKeyState::Down, 4)),
        LabelDisposition::Modifier
    );
    assert_eq!(
        filter.process(modifier_event("keyboard-a", 54, RawKeyState::Up, 5)),
        LabelDisposition::Modifier
    );
    assert_eq!(
        filter.process(modifier_event(
            "keyboard-a",
            LEFT_SHIFT_PHYSICAL_CODE,
            RawKeyState::Repeat,
            6,
        )),
        LabelDisposition::Rejected(LabelRejection::Repeat)
    );
    assert_eq!(
        filter.process(modifier_event(
            "keyboard-a",
            LEFT_SHIFT_PHYSICAL_CODE,
            RawKeyState::Up,
            7,
        )),
        LabelDisposition::Rejected(LabelRejection::OrphanRelease)
    );

    assert_eq!(
        filter.process(modifier_event(
            "keyboard-a",
            LEFT_SHIFT_PHYSICAL_CODE,
            RawKeyState::Down,
            8,
        )),
        LabelDisposition::Modifier
    );
    assert_eq!(
        filter.process(event("keyboard-a", 30, Some("A"), RawKeyState::Down, 9)),
        LabelDisposition::Rejected(LabelRejection::OverlappingPress)
    );
    assert_eq!(
        filter.process(event("keyboard-a", 30, Some("A"), RawKeyState::Up, 10)),
        LabelDisposition::Rejected(LabelRejection::KeyboardNotIdentified)
    );
    assert_eq!(
        filter.process(modifier_event(
            "keyboard-a",
            LEFT_SHIFT_PHYSICAL_CODE,
            RawKeyState::Up,
            11,
        )),
        LabelDisposition::Modifier
    );
    assert_eq!(
        filter.identification_state(),
        KeyboardIdentificationState::AwaitingLeftShiftDown
    );

    identify(&mut filter);
    assert!(matches!(
        filter.process(event("keyboard-a", 30, Some("A"), RawKeyState::Down, 12)),
        LabelDisposition::Accepted(_)
    ));
}

#[test]
fn deterministic_fake_source_uses_the_injected_shared_host_clock() {
    let mut source = FakeKeyboardEventSource::new([
        modifier_event("keyboard-a", LEFT_SHIFT_PHYSICAL_CODE, RawKeyState::Down, 0),
        modifier_event("keyboard-a", LEFT_SHIFT_PHYSICAL_CODE, RawKeyState::Up, 0),
        event("keyboard-a", 30, Some("A"), RawKeyState::Down, 0),
        event("keyboard-a", 30, Some("A"), RawKeyState::Up, 0),
    ]);
    let selected_identity = source.identity.clone();
    let mut filter = KeyboardLabelFilter::from_source(&source);
    let clock = FakeMonotonicClock::new([10, 20, 30, 40]);

    let source_seam: &mut dyn KeyboardEventSource = &mut source;
    assert_eq!(
        filter
            .read_next_from(source_seam, &clock)
            .expect("left Shift down reads"),
        LabelDisposition::Modifier
    );
    assert_eq!(
        filter
            .read_next_from(source_seam, &clock)
            .expect("left Shift up reads"),
        LabelDisposition::Modifier
    );
    let down = filter
        .read_next_from(source_seam, &clock)
        .expect("label down reads");
    assert!(matches!(
        down,
        LabelDisposition::Accepted(label)
            if label.host_time_ns == 30 && label.kernel_time_ns == Some(9_000_000_000)
    ));
    let up = filter
        .read_next_from(source_seam, &clock)
        .expect("label up reads");
    assert!(matches!(
        up,
        LabelDisposition::Accepted(label) if label.host_time_ns == 40
    ));
    assert_eq!(filter.selected_source_identity(), Some(&selected_identity));
}

#[derive(Clone, Copy)]
enum IdentityMutation {
    DeviceId,
    Name,
    PhysicalPath,
    Vendor,
    Product,
}

impl IdentityMutation {
    fn apply(self, identity: &mut KeyboardSourceIdentity) {
        match self {
            Self::DeviceId => identity.device_id.push_str("-changed"),
            Self::Name => identity.name.push_str(" changed"),
            Self::PhysicalPath => identity.physical_path.push_str("-changed"),
            Self::Vendor => identity.vendor_id ^= 1,
            Self::Product => identity.product_id ^= 1,
        }
    }
}

#[test]
fn source_identity_is_frozen_and_every_identity_mutation_disarms_labels() {
    for mutation in [
        IdentityMutation::DeviceId,
        IdentityMutation::Name,
        IdentityMutation::PhysicalPath,
        IdentityMutation::Vendor,
        IdentityMutation::Product,
    ] {
        let mut source = FakeKeyboardEventSource::new([
            modifier_event("keyboard-a", LEFT_SHIFT_PHYSICAL_CODE, RawKeyState::Down, 0),
            modifier_event("keyboard-a", LEFT_SHIFT_PHYSICAL_CODE, RawKeyState::Up, 0),
            event("keyboard-a", 30, Some("A"), RawKeyState::Down, 0),
        ]);
        let frozen = source.identity.clone();
        let artifact_identity =
            frozen.artifact_identity(KeyboardLayoutConfirmationReceipt::us_qwerty(1, 1));
        let mut filter = KeyboardLabelFilter::from_source(&source);
        let clock = FakeMonotonicClock::new([1, 2, 3]);

        filter
            .read_next_from(&mut source, &clock)
            .expect("left Shift down reads");
        filter
            .read_next_from(&mut source, &clock)
            .expect("left Shift up reads");
        mutation.apply(&mut source.identity);
        assert_eq!(
            filter
                .read_next_from(&mut source, &clock)
                .expect("identity mutation is a label rejection"),
            LabelDisposition::Rejected(LabelRejection::SourceIdentityChanged)
        );
        assert_eq!(
            filter.identification_state(),
            KeyboardIdentificationState::AwaitingLeftShiftDown
        );
        assert_eq!(filter.selected_source_identity(), Some(&frozen));
        assert_eq!(artifact_identity.physical_path, frozen.physical_path);
        assert_eq!(artifact_identity.vendor_id, frozen.vendor_id);
        assert_eq!(artifact_identity.product_id, frozen.product_id);
    }
}

#[test]
fn identity_change_during_a_source_read_cannot_leak_an_event() {
    let mut source = FakeKeyboardEventSource::new([modifier_event(
        "keyboard-a",
        LEFT_SHIFT_PHYSICAL_CODE,
        RawKeyState::Down,
        0,
    )]);
    let mut changed = source.identity.clone();
    changed.vendor_id ^= 1;
    source.identity_after_next_read = Some(changed);
    let mut filter = KeyboardLabelFilter::from_source(&source);

    assert_eq!(
        filter
            .read_next_from(&mut source, &FakeMonotonicClock::new([10]))
            .expect("source mutation becomes a rejection"),
        LabelDisposition::Rejected(LabelRejection::SourceIdentityChanged)
    );
    assert_eq!(
        filter.identification_state(),
        KeyboardIdentificationState::AwaitingLeftShiftDown
    );
}

#[cfg(target_os = "linux")]
#[test]
fn linux_device_exposes_the_same_production_event_source_seam() {
    use invisible_keyboard::labels::{HostTimeOrigin, LinuxKeyboardDevice};

    fn assert_event_source<T: KeyboardEventSource>() {}
    fn assert_host_clock<T: MonotonicHostClock>() {}

    assert_event_source::<LinuxKeyboardDevice>();
    assert_host_clock::<HostTimeOrigin>();
    let production_read: fn(
        &mut LinuxKeyboardDevice,
        &dyn MonotonicHostClock,
    ) -> io::Result<RawKeyboardEvent> = LinuxKeyboardDevice::read_next;
    let _ = production_read;
}

#[test]
fn alignment_is_one_to_one_in_the_shared_host_clock_domain() {
    let left_fit = fit(1, 1_000_000_000);
    let right_fit = fit(2, 2_000_000_000);
    let aligner = LabelAligner::new(left_fit, Some(right_fit));
    let mut filter = KeyboardLabelFilter::new("keyboard-a");
    identify(&mut filter);
    let labels = [
        event(
            "keyboard-a",
            30,
            Some("A"),
            RawKeyState::Down,
            1_050_000_000,
        ),
        event("keyboard-a", 30, Some("A"), RawKeyState::Up, 1_060_000_000),
        event(
            "keyboard-a",
            36,
            Some("J"),
            RawKeyState::Down,
            2_100_000_000,
        ),
    ]
    .into_iter()
    .filter_map(|event| match filter.process(event) {
        LabelDisposition::Accepted(label) if label.state == RawKeyState::Down => Some(label),
        _ => None,
    })
    .collect::<Vec<_>>();
    let impulses = vec![
        SensorImpulse {
            hand: Hand::Left,
            boot_id: [1; 16],
            device_time_us: 55_000,
            ordinal: 10,
        },
        SensorImpulse {
            hand: Hand::Left,
            boot_id: [1; 16],
            device_time_us: 60_000,
            ordinal: 11,
        },
        SensorImpulse {
            hand: Hand::Right,
            boot_id: [2; 16],
            device_time_us: 105_000,
            ordinal: 12,
        },
    ];

    let report = aligner
        .align(&labels, &impulses)
        .expect("alignment succeeds");
    assert_eq!(report.matches.len(), 2);
    assert_eq!(report.matches[0].impulse_ordinal, 10);
    assert_eq!(report.matches[1].impulse_ordinal, 12);
    assert_eq!(report.unmatched_impulse_ordinals, vec![11]);
    assert!(report.unmatched_label_indices.is_empty());
}

#[test]
fn impulses_outside_the_frozen_window_remain_unmatched() {
    let aligner = LabelAligner::new(fit(1, 1_000_000_000), None);
    let mut filter = KeyboardLabelFilter::new("keyboard-a");
    identify(&mut filter);
    let label = match filter.process(event(
        "keyboard-a",
        30,
        Some("A"),
        RawKeyState::Down,
        1_050_000_000,
    )) {
        LabelDisposition::Accepted(label) => label,
        other => panic!("unexpected filter result: {other:?}"),
    };
    let impulses = [SensorImpulse {
        hand: Hand::Left,
        boot_id: [1; 16],
        device_time_us: 400_000,
        ordinal: 1,
    }];

    let report = aligner
        .align(&[label], &impulses)
        .expect("alignment succeeds");
    assert!(report.matches.is_empty());
    assert_eq!(report.unmatched_label_indices, vec![0]);
    assert_eq!(report.unmatched_impulse_ordinals, vec![1]);
}
