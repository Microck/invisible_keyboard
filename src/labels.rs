//! Selected-keyboard labels, one-to-one sensor alignment, and Linux EV_KEY input.

use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt, io,
    time::Instant,
};

use serde::{Deserialize, Serialize};

use crate::{
    artifact::{KeyboardIdentity, KeyboardLayoutConfirmationReceipt},
    clock::{ClockError, ClockFit},
    layout::{Hand, PhysicalPosition},
};

pub const INTENT_WINDOW_START_NS: i64 = -50_000_000;
pub const INTENT_WINDOW_END_NS: i64 = 250_000_000;
pub const LEFT_SHIFT_PHYSICAL_CODE: u16 = 42;

/// Stable facts frozen when one keyboard source is selected.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KeyboardSourceIdentity {
    pub device_id: String,
    pub name: String,
    pub physical_path: String,
    pub vendor_id: u16,
    pub product_id: u16,
}

impl KeyboardSourceIdentity {
    #[must_use]
    pub fn artifact_identity(
        &self,
        layout_confirmation: KeyboardLayoutConfirmationReceipt,
    ) -> KeyboardIdentity {
        KeyboardIdentity {
            name: self.name.clone(),
            physical_path: self.physical_path.clone(),
            vendor_id: self.vendor_id,
            product_id: self.product_id,
            logical_layout: layout_confirmation.layout_id.clone(),
            layout_confirmation,
        }
    }
}

/// The one process-relative monotonic clock shared by keyboard and sensor I/O.
pub trait MonotonicHostClock {
    fn now_ns(&self) -> u64;
}

/// Process-relative clock origin. Clones retain the same origin.
#[derive(Clone, Debug)]
pub struct HostTimeOrigin {
    started: Instant,
}

impl Default for HostTimeOrigin {
    fn default() -> Self {
        Self::new()
    }
}

impl HostTimeOrigin {
    #[must_use]
    pub fn new() -> Self {
        Self {
            started: Instant::now(),
        }
    }

    #[must_use]
    pub fn now_ns(&self) -> u64 {
        MonotonicHostClock::now_ns(self)
    }
}

impl MonotonicHostClock for HostTimeOrigin {
    fn now_ns(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_nanos()).unwrap_or(u64::MAX)
    }
}

/// Injectable selected-keyboard input boundary used by production and fakes.
pub trait KeyboardEventSource {
    fn identity(&self) -> KeyboardSourceIdentity;

    /// Read one physical-key event and timestamp receipt with `host_clock`.
    fn read_event(&mut self, host_clock: &dyn MonotonicHostClock) -> io::Result<RawKeyboardEvent>;
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RawKeyState {
    Down,
    Up,
    Repeat,
}

/// Raw facts preserved from one selected or rejected physical-key event.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RawKeyboardEvent {
    pub device_id: String,
    pub physical_code: u16,
    pub position: Option<PhysicalPosition>,
    pub modifier: bool,
    pub state: RawKeyState,
    pub kernel_time_ns: Option<u64>,
    pub host_time_ns: u64,
}

/// Accepted event that may enter prompt progress or alignment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct KeyboardLabel {
    pub physical_code: u16,
    pub position: PhysicalPosition,
    pub state: RawKeyState,
    pub kernel_time_ns: Option<u64>,
    pub host_time_ns: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LabelRejection {
    WrongKeyboard,
    KeyboardNotIdentified,
    SourceIdentityChanged,
    Repeat,
    OverlappingPress,
    ModifierCombination,
    UnsupportedKey,
    OrphanRelease,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LabelDisposition {
    Accepted(KeyboardLabel),
    Rejected(LabelRejection),
    Modifier,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KeyboardIdentificationState {
    AwaitingLeftShiftDown,
    AwaitingLeftShiftUp,
    Identified,
}

#[derive(Clone, Copy, Debug)]
enum HeldNonmodifier {
    Accepted(PhysicalPosition),
    Rejected(LabelRejection),
}

/// Stateful boundary filter for exactly one physical keyboard.
#[derive(Clone, Debug)]
pub struct KeyboardLabelFilter {
    selected_device_id: String,
    selected_source_identity: Option<KeyboardSourceIdentity>,
    identification_state: KeyboardIdentificationState,
    identification_held: BTreeSet<u16>,
    held_modifiers: BTreeSet<u16>,
    held_nonmodifiers: BTreeMap<u16, HeldNonmodifier>,
}

impl KeyboardLabelFilter {
    #[must_use]
    pub fn new(selected_device_id: impl Into<String>) -> Self {
        Self {
            selected_device_id: selected_device_id.into(),
            selected_source_identity: None,
            identification_state: KeyboardIdentificationState::AwaitingLeftShiftDown,
            identification_held: BTreeSet::new(),
            held_modifiers: BTreeSet::new(),
            held_nonmodifiers: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn from_source(source: &(impl KeyboardEventSource + ?Sized)) -> Self {
        let identity = source.identity();
        let mut filter = Self::new(identity.device_id.clone());
        filter.selected_source_identity = Some(identity);
        filter
    }

    #[must_use]
    pub const fn identification_state(&self) -> KeyboardIdentificationState {
        self.identification_state
    }

    #[must_use]
    pub const fn is_identified(&self) -> bool {
        matches!(
            self.identification_state,
            KeyboardIdentificationState::Identified
        )
    }

    #[must_use]
    pub const fn selected_source_identity(&self) -> Option<&KeyboardSourceIdentity> {
        self.selected_source_identity.as_ref()
    }

    /// Read and process one event while enforcing the frozen source identity.
    pub fn read_next_from(
        &mut self,
        source: &mut (impl KeyboardEventSource + ?Sized),
        host_clock: &dyn MonotonicHostClock,
    ) -> io::Result<LabelDisposition> {
        if !self.source_identity_matches(source) {
            self.disarm_for_identity_change();
            return Ok(LabelDisposition::Rejected(
                LabelRejection::SourceIdentityChanged,
            ));
        }
        let event = source.read_event(host_clock)?;
        if !self.source_identity_matches(source) {
            self.disarm_for_identity_change();
            return Ok(LabelDisposition::Rejected(
                LabelRejection::SourceIdentityChanged,
            ));
        }
        Ok(self.process(event))
    }

    /// Preserve state while deciding whether an event may advance coverage.
    pub fn process(&mut self, event: RawKeyboardEvent) -> LabelDisposition {
        if event.device_id != self.selected_device_id {
            return LabelDisposition::Rejected(LabelRejection::WrongKeyboard);
        }
        if event.state == RawKeyState::Repeat {
            if self.identification_state == KeyboardIdentificationState::AwaitingLeftShiftUp {
                self.identification_state = KeyboardIdentificationState::AwaitingLeftShiftDown;
            }
            return LabelDisposition::Rejected(LabelRejection::Repeat);
        }
        if !self.is_identified() {
            return self.process_identification(event);
        }
        if event.modifier {
            return self.process_modifier(event);
        }
        self.process_nonmodifier(event)
    }

    fn process_identification(&mut self, event: RawKeyboardEvent) -> LabelDisposition {
        match event.state {
            RawKeyState::Down => {
                let had_held_key = !self.identification_held.is_empty();
                let newly_held = self.identification_held.insert(event.physical_code);
                if had_held_key || !newly_held {
                    self.identification_state = KeyboardIdentificationState::AwaitingLeftShiftDown;
                    return LabelDisposition::Rejected(LabelRejection::OverlappingPress);
                }
                if is_left_shift(&event) {
                    self.identification_state = KeyboardIdentificationState::AwaitingLeftShiftUp;
                    LabelDisposition::Modifier
                } else if event.modifier {
                    LabelDisposition::Modifier
                } else {
                    LabelDisposition::Rejected(LabelRejection::KeyboardNotIdentified)
                }
            }
            RawKeyState::Up => {
                let was_waiting_for_left_shift_up =
                    self.identification_state == KeyboardIdentificationState::AwaitingLeftShiftUp;
                if !self.identification_held.remove(&event.physical_code) {
                    if is_left_shift(&event) {
                        self.identification_state =
                            KeyboardIdentificationState::AwaitingLeftShiftDown;
                    }
                    return LabelDisposition::Rejected(LabelRejection::OrphanRelease);
                }
                if is_left_shift(&event)
                    && was_waiting_for_left_shift_up
                    && self.identification_held.is_empty()
                {
                    self.identification_state = KeyboardIdentificationState::Identified;
                    return LabelDisposition::Modifier;
                }
                self.identification_state = KeyboardIdentificationState::AwaitingLeftShiftDown;
                if event.modifier {
                    LabelDisposition::Modifier
                } else {
                    LabelDisposition::Rejected(LabelRejection::KeyboardNotIdentified)
                }
            }
            RawKeyState::Repeat => unreachable!("repeat returned before state handling"),
        }
    }

    fn process_modifier(&mut self, event: RawKeyboardEvent) -> LabelDisposition {
        match event.state {
            RawKeyState::Down => {
                let had_held_key =
                    !self.held_modifiers.is_empty() || !self.held_nonmodifiers.is_empty();
                let newly_held = self.held_modifiers.insert(event.physical_code);
                if !newly_held {
                    self.reject_held_nonmodifiers(LabelRejection::OverlappingPress);
                    return LabelDisposition::Rejected(LabelRejection::OverlappingPress);
                }
                if had_held_key {
                    self.reject_held_nonmodifiers(LabelRejection::ModifierCombination);
                    LabelDisposition::Rejected(LabelRejection::ModifierCombination)
                } else {
                    LabelDisposition::Modifier
                }
            }
            RawKeyState::Up => {
                if !self.held_modifiers.remove(&event.physical_code) {
                    return LabelDisposition::Rejected(LabelRejection::OrphanRelease);
                }
                if !self.held_modifiers.is_empty() || !self.held_nonmodifiers.is_empty() {
                    self.reject_held_nonmodifiers(LabelRejection::ModifierCombination);
                    LabelDisposition::Rejected(LabelRejection::ModifierCombination)
                } else {
                    LabelDisposition::Modifier
                }
            }
            RawKeyState::Repeat => unreachable!("repeat returned before state handling"),
        }
    }

    fn process_nonmodifier(&mut self, event: RawKeyboardEvent) -> LabelDisposition {
        match event.state {
            RawKeyState::Down => {
                if self.held_nonmodifiers.contains_key(&event.physical_code) {
                    self.reject_held_nonmodifiers(LabelRejection::OverlappingPress);
                    return LabelDisposition::Rejected(LabelRejection::OverlappingPress);
                }
                if !self.held_nonmodifiers.is_empty() {
                    self.reject_held_nonmodifiers(LabelRejection::OverlappingPress);
                    self.held_nonmodifiers.insert(
                        event.physical_code,
                        HeldNonmodifier::Rejected(LabelRejection::OverlappingPress),
                    );
                    return LabelDisposition::Rejected(LabelRejection::OverlappingPress);
                }
                if !self.held_modifiers.is_empty() {
                    self.held_nonmodifiers.insert(
                        event.physical_code,
                        HeldNonmodifier::Rejected(LabelRejection::ModifierCombination),
                    );
                    return LabelDisposition::Rejected(LabelRejection::ModifierCombination);
                }
                let Some(position) = event.position else {
                    self.held_nonmodifiers.insert(
                        event.physical_code,
                        HeldNonmodifier::Rejected(LabelRejection::UnsupportedKey),
                    );
                    return LabelDisposition::Rejected(LabelRejection::UnsupportedKey);
                };
                self.held_nonmodifiers
                    .insert(event.physical_code, HeldNonmodifier::Accepted(position));
                LabelDisposition::Accepted(label_from_event(event, position))
            }
            RawKeyState::Up => {
                let Some(held) = self.held_nonmodifiers.remove(&event.physical_code) else {
                    return LabelDisposition::Rejected(LabelRejection::OrphanRelease);
                };
                match held {
                    HeldNonmodifier::Rejected(reason) => LabelDisposition::Rejected(reason),
                    HeldNonmodifier::Accepted(position) => {
                        if !self.held_modifiers.is_empty() {
                            return LabelDisposition::Rejected(LabelRejection::ModifierCombination);
                        }
                        if event.position != Some(position) {
                            return LabelDisposition::Rejected(LabelRejection::UnsupportedKey);
                        }
                        LabelDisposition::Accepted(label_from_event(event, position))
                    }
                }
            }
            RawKeyState::Repeat => unreachable!("repeat returned before state handling"),
        }
    }

    fn reject_held_nonmodifiers(&mut self, reason: LabelRejection) {
        for held in self.held_nonmodifiers.values_mut() {
            *held = HeldNonmodifier::Rejected(reason);
        }
    }

    fn source_identity_matches(&self, source: &(impl KeyboardEventSource + ?Sized)) -> bool {
        self.selected_source_identity
            .as_ref()
            .is_some_and(|selected| source.identity() == *selected)
    }

    fn disarm_for_identity_change(&mut self) {
        self.identification_state = KeyboardIdentificationState::AwaitingLeftShiftDown;
        self.identification_held.clear();
        self.held_modifiers.clear();
        self.held_nonmodifiers.clear();
    }

    #[must_use]
    pub fn held_count(&self) -> usize {
        if self.is_identified() {
            self.held_modifiers.len() + self.held_nonmodifiers.len()
        } else {
            self.identification_held.len()
        }
    }
}

fn is_left_shift(event: &RawKeyboardEvent) -> bool {
    event.modifier && event.physical_code == LEFT_SHIFT_PHYSICAL_CODE
}

fn label_from_event(event: RawKeyboardEvent, position: PhysicalPosition) -> KeyboardLabel {
    KeyboardLabel {
        physical_code: event.physical_code,
        position,
        state: event.state,
        kernel_time_ns: event.kernel_time_ns,
        host_time_ns: event.host_time_ns,
    }
}

/// One detected sensor impulse before a physical-keyboard label is assigned.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SensorImpulse {
    pub hand: Hand,
    pub boot_id: [u8; 16],
    pub device_time_us: u64,
    pub ordinal: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AlignedExample {
    pub label_index: usize,
    pub position: PhysicalPosition,
    pub impulse_ordinal: u64,
    pub key_host_time_ns: u64,
    pub impulse_host_time_ns: u64,
    pub offset_ns: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AlignmentReport {
    pub matches: Vec<AlignedExample>,
    pub unmatched_label_indices: Vec<usize>,
    pub unmatched_impulse_ordinals: Vec<u64>,
}

/// Frozen clock mappings and one-to-one broad-window matching.
#[derive(Clone, Copy, Debug)]
pub struct LabelAligner {
    left: ClockFit,
    right: Option<ClockFit>,
}

impl LabelAligner {
    #[must_use]
    pub const fn new(left: ClockFit, right: Option<ClockFit>) -> Self {
        Self { left, right }
    }

    /// Greedily select the nearest unused same-hand impulse for each key-down.
    ///
    /// The broad window is frozen here. Offset estimation and tighter matching
    /// must use a training block, then lock before held-out scoring.
    pub fn align(
        &self,
        labels: &[KeyboardLabel],
        impulses: &[SensorImpulse],
    ) -> Result<AlignmentReport, LabelAlignmentError> {
        // Impulses are dense slice indices. A bitmap keeps membership and the
        // final unmatched pass linear without changing greedy match order.
        let mut used_impulses = vec![false; impulses.len()];
        let mut mapped_host_times = vec![None; impulses.len()];
        let mut matches = Vec::new();
        let mut unmatched_label_indices = Vec::new();

        for (label_index, label) in labels.iter().enumerate() {
            if label.state != RawKeyState::Down {
                continue;
            }
            let mut best: Option<(usize, u64, i64, u64)> = None;
            for (impulse_index, impulse) in impulses.iter().enumerate() {
                if used_impulses[impulse_index] || impulse.hand != label.position.hand {
                    continue;
                }
                let impulse_host_time_ns = if let Some(mapped) = mapped_host_times[impulse_index] {
                    mapped
                } else {
                    let mapped = self
                        .fit_for(impulse.hand)?
                        .map_device_time_ns(impulse.boot_id, impulse.device_time_us)
                        .map_err(LabelAlignmentError::Clock)?;
                    mapped_host_times[impulse_index] = Some(mapped);
                    mapped
                };
                let raw_offset = i128::from(impulse_host_time_ns) - i128::from(label.host_time_ns);
                let Ok(offset_ns) = i64::try_from(raw_offset) else {
                    continue;
                };
                if !(INTENT_WINDOW_START_NS..=INTENT_WINDOW_END_NS).contains(&offset_ns) {
                    continue;
                }
                let distance = offset_ns.unsigned_abs();
                if best.is_none_or(|(_, best_distance, _, _)| distance < best_distance) {
                    best = Some((impulse_index, distance, offset_ns, impulse_host_time_ns));
                }
            }

            let Some((impulse_index, _, offset_ns, impulse_host_time_ns)) = best else {
                unmatched_label_indices.push(label_index);
                continue;
            };
            used_impulses[impulse_index] = true;
            let impulse = impulses[impulse_index];
            matches.push(AlignedExample {
                label_index,
                position: label.position,
                impulse_ordinal: impulse.ordinal,
                key_host_time_ns: label.host_time_ns,
                impulse_host_time_ns,
                offset_ns,
            });
        }

        let unmatched_impulse_ordinals = impulses
            .iter()
            .enumerate()
            .filter_map(|(index, impulse)| (!used_impulses[index]).then_some(impulse.ordinal))
            .collect();
        Ok(AlignmentReport {
            matches,
            unmatched_label_indices,
            unmatched_impulse_ordinals,
        })
    }

    fn fit_for(&self, hand: Hand) -> Result<&ClockFit, LabelAlignmentError> {
        match hand {
            Hand::Left => Ok(&self.left),
            Hand::Right => self
                .right
                .as_ref()
                .ok_or(LabelAlignmentError::MissingClockFit(Hand::Right)),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LabelAlignmentError {
    MissingClockFit(Hand),
    Clock(ClockError),
}

impl fmt::Display for LabelAlignmentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingClockFit(hand) => write!(formatter, "no clock fit exists for {hand:?}"),
            Self::Clock(error) => write!(formatter, "label alignment clock failed: {error}"),
        }
    }
}

impl Error for LabelAlignmentError {}

#[cfg(target_os = "linux")]
mod linux {
    use std::{
        collections::VecDeque,
        io,
        path::{Path, PathBuf},
        time::UNIX_EPOCH,
    };

    use evdev::{Device, EventSummary, InputEvent, KeyCode};

    use super::{
        KeyboardEventSource, KeyboardSourceIdentity, MonotonicHostClock, RawKeyState,
        RawKeyboardEvent,
    };
    use crate::layout::{KeyUsage, PhysicalPosition, position_for_label, position_for_usage};

    #[derive(Clone, Debug, Eq, PartialEq)]
    pub struct LinuxKeyboardDescriptor {
        pub device_id: String,
        pub path: PathBuf,
        pub name: String,
        pub physical_path: String,
        pub vendor_id: u16,
        pub product_id: u16,
    }

    impl LinuxKeyboardDescriptor {
        #[must_use]
        pub fn source_identity(&self) -> KeyboardSourceIdentity {
            KeyboardSourceIdentity {
                device_id: self.device_id.clone(),
                name: self.name.clone(),
                physical_path: self.physical_path.clone(),
                vendor_id: self.vendor_id,
                product_id: self.product_id,
            }
        }

        #[must_use]
        pub fn artifact_identity(
            &self,
            layout_confirmation: crate::artifact::KeyboardLayoutConfirmationReceipt,
        ) -> crate::artifact::KeyboardIdentity {
            self.source_identity()
                .artifact_identity(layout_confirmation)
        }
    }

    /// Blocking selected-device reader. It does not grab the keyboard.
    pub struct LinuxKeyboardDevice {
        descriptor: LinuxKeyboardDescriptor,
        device: Device,
        pending: VecDeque<InputEvent>,
    }

    impl LinuxKeyboardDevice {
        pub fn open(path: &Path) -> io::Result<Self> {
            let device = Device::open(path)?;
            if !is_keyboard(&device) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "selected evdev node is not a keyboard",
                ));
            }
            let descriptor = descriptor(path.to_path_buf(), &device);
            Ok(Self {
                descriptor,
                device,
                pending: VecDeque::new(),
            })
        }

        #[must_use]
        pub const fn descriptor(&self) -> &LinuxKeyboardDescriptor {
            &self.descriptor
        }

        /// Query the kernel's current held-key state before event delivery.
        ///
        /// The event reader starts from this snapshot so a key held before the
        /// evdev node opens cannot disappear at a calibration-worker boundary.
        pub fn held_key_codes(&self) -> io::Result<Vec<u16>> {
            Ok(self
                .device
                .get_key_state()?
                .iter()
                .map(|key| key.0)
                .collect())
        }

        /// Read the next EV_KEY event, preserving the kernel and host receipt
        /// timestamps. Non-key events remain irrelevant to label semantics.
        pub fn read_next(
            &mut self,
            host_clock: &dyn MonotonicHostClock,
        ) -> io::Result<RawKeyboardEvent> {
            loop {
                if self.pending.is_empty() {
                    self.pending.extend(self.device.fetch_events()?);
                }
                let event = self
                    .pending
                    .pop_front()
                    .expect("blocking fetch returns at least one event");
                let kernel_time_ns = event
                    .timestamp()
                    .duration_since(UNIX_EPOCH)
                    .ok()
                    .and_then(|duration| u64::try_from(duration.as_nanos()).ok());
                let EventSummary::Key(_, key, value) = event.destructure() else {
                    continue;
                };
                let state = match value {
                    0 => RawKeyState::Up,
                    1 => RawKeyState::Down,
                    2 => RawKeyState::Repeat,
                    _ => continue,
                };
                return Ok(RawKeyboardEvent {
                    device_id: self.descriptor.device_id.clone(),
                    physical_code: key.0,
                    position: position_for_key(key),
                    modifier: is_modifier(key),
                    state,
                    kernel_time_ns,
                    host_time_ns: host_clock.now_ns(),
                });
            }
        }
    }

    impl KeyboardEventSource for LinuxKeyboardDevice {
        fn identity(&self) -> KeyboardSourceIdentity {
            self.descriptor.source_identity()
        }

        fn read_event(
            &mut self,
            host_clock: &dyn MonotonicHostClock,
        ) -> io::Result<RawKeyboardEvent> {
            self.read_next(host_clock)
        }
    }

    /// Enumerate readable keyboard nodes. Omitted nodes remain explicit host
    /// permission or device-discovery failures in the UI.
    #[must_use]
    pub fn list_linux_keyboards() -> Vec<LinuxKeyboardDescriptor> {
        evdev::enumerate()
            .filter(|(_, device)| is_keyboard(device))
            .map(|(path, device)| descriptor(path, &device))
            .collect()
    }

    fn descriptor(path: PathBuf, device: &Device) -> LinuxKeyboardDescriptor {
        let input_id = device.input_id();
        let physical_path = device.physical_path().unwrap_or("unknown").to_owned();
        LinuxKeyboardDescriptor {
            device_id: path.display().to_string(),
            path,
            name: device.name().unwrap_or("Unnamed keyboard").to_owned(),
            physical_path,
            vendor_id: input_id.vendor(),
            product_id: input_id.product(),
        }
    }

    fn is_keyboard(device: &Device) -> bool {
        device.supported_keys().is_some_and(|keys| {
            keys.contains(KeyCode::KEY_A)
                && keys.contains(KeyCode::KEY_Z)
                && keys.contains(KeyCode::KEY_SPACE)
                && keys.contains(KeyCode::KEY_ENTER)
        })
    }

    fn position_for_key(key: KeyCode) -> Option<PhysicalPosition> {
        let usage = match key {
            KeyCode::KEY_A => Some(KeyUsage::A),
            KeyCode::KEY_B => Some(KeyUsage::B),
            KeyCode::KEY_C => Some(KeyUsage::C),
            KeyCode::KEY_D => Some(KeyUsage::D),
            KeyCode::KEY_E => Some(KeyUsage::E),
            KeyCode::KEY_F => Some(KeyUsage::F),
            KeyCode::KEY_G => Some(KeyUsage::G),
            KeyCode::KEY_H => Some(KeyUsage::H),
            KeyCode::KEY_I => Some(KeyUsage::I),
            KeyCode::KEY_J => Some(KeyUsage::J),
            KeyCode::KEY_K => Some(KeyUsage::K),
            KeyCode::KEY_L => Some(KeyUsage::L),
            KeyCode::KEY_M => Some(KeyUsage::M),
            KeyCode::KEY_N => Some(KeyUsage::N),
            KeyCode::KEY_O => Some(KeyUsage::O),
            KeyCode::KEY_P => Some(KeyUsage::P),
            KeyCode::KEY_Q => Some(KeyUsage::Q),
            KeyCode::KEY_R => Some(KeyUsage::R),
            KeyCode::KEY_S => Some(KeyUsage::S),
            KeyCode::KEY_T => Some(KeyUsage::T),
            KeyCode::KEY_U => Some(KeyUsage::U),
            KeyCode::KEY_V => Some(KeyUsage::V),
            KeyCode::KEY_W => Some(KeyUsage::W),
            KeyCode::KEY_X => Some(KeyUsage::X),
            KeyCode::KEY_Y => Some(KeyUsage::Y),
            KeyCode::KEY_Z => Some(KeyUsage::Z),
            KeyCode::KEY_SPACE => Some(KeyUsage::Space),
            KeyCode::KEY_BACKSPACE => Some(KeyUsage::Backspace),
            _ => None,
        };
        usage.and_then(position_for_usage).or_else(|| match key {
            KeyCode::KEY_COMMA => position_for_label("comma"),
            KeyCode::KEY_DOT => position_for_label("period"),
            KeyCode::KEY_SEMICOLON => position_for_label("semicolon"),
            KeyCode::KEY_SLASH => position_for_label("slash"),
            _ => None,
        })
    }

    const fn is_modifier(key: KeyCode) -> bool {
        matches!(
            key,
            KeyCode::KEY_LEFTSHIFT
                | KeyCode::KEY_RIGHTSHIFT
                | KeyCode::KEY_LEFTCTRL
                | KeyCode::KEY_RIGHTCTRL
                | KeyCode::KEY_LEFTALT
                | KeyCode::KEY_RIGHTALT
                | KeyCode::KEY_LEFTMETA
                | KeyCode::KEY_RIGHTMETA
        )
    }
}

#[cfg(target_os = "linux")]
pub use linux::{LinuxKeyboardDescriptor, LinuxKeyboardDevice, list_linux_keyboards};
