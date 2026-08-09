//! Physical 34-position geometry and the conservative Core-28 key manifest.

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

/// Stable identity for the one enabled Core-28 layout on the 34-position grid.
pub const CORE28_LAYOUT_ID: &str = "invisible-keyboard-core28-v1";

/// Schema for the canonical, human-inspectable layout byte sequence.
pub const CORE28_LAYOUT_MANIFEST_SCHEMA: &str = "invisible-keyboard-layout-manifest-v1";

/// Total positions in this project's independently defined two-hand topology.
/// Its 34-slot count follows the externally documented reference arrangement.
pub const PHYSICAL_POSITION_COUNT: usize = 34;

/// Enabled key usages in the first product manifest.
pub const CORE28_KEY_COUNT: usize = 28;

/// Physical positions intentionally disabled in Core-28.
pub const RESERVED_POSITION_COUNT: usize = 6;

/// The hand that owns a sensor channel or physical key position.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "lowercase")]
#[repr(u8)]
pub enum Hand {
    /// Left-hand wearable.
    Left = 0,
    /// Right-hand wearable.
    Right = 1,
}

impl Hand {
    /// Return this hand's fixed index in left-then-right two-hand arrays.
    #[must_use]
    pub(crate) const fn index(self) -> usize {
        match self {
            Self::Left => 0,
            Self::Right => 1,
        }
    }
}

impl TryFrom<u8> for Hand {
    type Error = u8;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Left),
            1 => Ok(Self::Right),
            invalid => Err(invalid),
        }
    }
}

/// A finger sensor that owns one or more physical positions.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Finger {
    /// Thumb ring or thumb-mounted sensor.
    Thumb,
    /// Index finger. It owns two three-position columns.
    Index,
    /// Middle finger.
    Middle,
    /// Ring finger.
    Ring,
    /// Pinky finger.
    Pinky,
}

impl Finger {
    /// Return this finger's canonical per-hand sensor index.
    pub(crate) const fn sensor_index(self) -> usize {
        match self {
            Self::Thumb => 0,
            Self::Index => 1,
            Self::Middle => 2,
            Self::Ring => 3,
            Self::Pinky => 4,
        }
    }

    /// Resolve a canonical finger sensor index. The dorsal sensor is not a
    /// finger and therefore returns `None`.
    pub(crate) const fn from_sensor_index(index: usize) -> Option<Self> {
        match index {
            0 => Some(Self::Thumb),
            1 => Some(Self::Index),
            2 => Some(Self::Middle),
            3 => Some(Self::Ring),
            4 => Some(Self::Pinky),
            _ => None,
        }
    }
}

/// One position in the fixed 34-position physical geometry.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PhysicalPosition {
    /// Owning hand.
    pub hand: Hand,
    /// Owning finger sensor.
    pub finger: Finger,
    /// Zero-based position within the finger's output group.
    pub slot: u8,
}

impl PhysicalPosition {
    const fn new(hand: Hand, finger: Finger, slot: u8) -> Self {
        Self { hand, finger, slot }
    }
}

/// Exact key usages advertised by Core-28.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub enum KeyUsage {
    A,
    B,
    C,
    D,
    E,
    F,
    G,
    H,
    I,
    J,
    K,
    L,
    M,
    N,
    O,
    P,
    Q,
    R,
    S,
    T,
    U,
    V,
    W,
    X,
    Y,
    Z,
    Space,
    Backspace,
}

impl KeyUsage {
    /// Convert an ASCII letter to its Core-28 usage.
    ///
    /// Lowercase and uppercase letters map to the same physical usage. Shift
    /// semantics are intentionally outside Core-28.
    #[must_use]
    pub const fn from_ascii_letter(character: char) -> Option<Self> {
        match character {
            'A' | 'a' => Some(Self::A),
            'B' | 'b' => Some(Self::B),
            'C' | 'c' => Some(Self::C),
            'D' | 'd' => Some(Self::D),
            'E' | 'e' => Some(Self::E),
            'F' | 'f' => Some(Self::F),
            'G' | 'g' => Some(Self::G),
            'H' | 'h' => Some(Self::H),
            'I' | 'i' => Some(Self::I),
            'J' | 'j' => Some(Self::J),
            'K' | 'k' => Some(Self::K),
            'L' | 'l' => Some(Self::L),
            'M' | 'm' => Some(Self::M),
            'N' | 'n' => Some(Self::N),
            'O' | 'o' => Some(Self::O),
            'P' | 'p' => Some(Self::P),
            'Q' | 'q' => Some(Self::Q),
            'R' | 'r' => Some(Self::R),
            'S' | 's' => Some(Self::S),
            'T' | 't' => Some(Self::T),
            'U' | 'u' => Some(Self::U),
            'V' | 'v' => Some(Self::V),
            'W' | 'w' => Some(Self::W),
            'X' | 'x' => Some(Self::X),
            'Y' | 'y' => Some(Self::Y),
            'Z' | 'z' => Some(Self::Z),
            _ => None,
        }
    }

    /// Return the printable ASCII character for letter and Space usages.
    /// Backspace is an event rather than text and therefore returns `None`.
    #[must_use]
    pub const fn as_ascii(self) -> Option<char> {
        match self {
            Self::A => Some('a'),
            Self::B => Some('b'),
            Self::C => Some('c'),
            Self::D => Some('d'),
            Self::E => Some('e'),
            Self::F => Some('f'),
            Self::G => Some('g'),
            Self::H => Some('h'),
            Self::I => Some('i'),
            Self::J => Some('j'),
            Self::K => Some('k'),
            Self::L => Some('l'),
            Self::M => Some('m'),
            Self::N => Some('n'),
            Self::O => Some('o'),
            Self::P => Some('p'),
            Self::Q => Some('q'),
            Self::R => Some('r'),
            Self::S => Some('s'),
            Self::T => Some('t'),
            Self::U => Some('u'),
            Self::V => Some('v'),
            Self::W => Some('w'),
            Self::X => Some('x'),
            Self::Y => Some('y'),
            Self::Z => Some('z'),
            Self::Space => Some(' '),
            Self::Backspace => None,
        }
    }
}

/// One physical position and its optional Core-28 usage.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct KeySlot {
    /// Fixed physical position.
    pub position: PhysicalPosition,
    /// Zero-based row in the canonical four-row presentation grid.
    pub display_row: u8,
    /// Zero-based column in the canonical eleven-column presentation grid.
    pub display_column: u8,
    /// Enabled key usage, or `None` when this position is reserved.
    pub usage: Option<KeyUsage>,
    /// Stable human-readable label for manifests and diagnostics.
    pub label: &'static str,
}

const fn slot(
    hand: Hand,
    finger: Finger,
    position: u8,
    display_row: u8,
    display_column: u8,
    usage: Option<KeyUsage>,
    label: &'static str,
) -> KeySlot {
    KeySlot {
        position: PhysicalPosition::new(hand, finger, position),
        display_row,
        display_column,
        usage,
        label,
    }
}

// Slots are grouped by hand and sensor. Index slots 0..=2 are the outer
// QWERTY column and 3..=5 are the inner column. This project-defined ordering
// is a stable manifest contract, not a reuse of the reference implementation.
const ALL_SLOTS: [KeySlot; PHYSICAL_POSITION_COUNT] = [
    slot(Hand::Left, Finger::Pinky, 0, 0, 0, Some(KeyUsage::Q), "Q"),
    slot(Hand::Left, Finger::Pinky, 1, 1, 0, Some(KeyUsage::A), "A"),
    slot(Hand::Left, Finger::Pinky, 2, 2, 0, Some(KeyUsage::Z), "Z"),
    slot(Hand::Left, Finger::Ring, 0, 0, 1, Some(KeyUsage::W), "W"),
    slot(Hand::Left, Finger::Ring, 1, 1, 1, Some(KeyUsage::S), "S"),
    slot(Hand::Left, Finger::Ring, 2, 2, 1, Some(KeyUsage::X), "X"),
    slot(Hand::Left, Finger::Middle, 0, 0, 2, Some(KeyUsage::E), "E"),
    slot(Hand::Left, Finger::Middle, 1, 1, 2, Some(KeyUsage::D), "D"),
    slot(Hand::Left, Finger::Middle, 2, 2, 2, Some(KeyUsage::C), "C"),
    slot(Hand::Left, Finger::Index, 0, 0, 3, Some(KeyUsage::R), "R"),
    slot(Hand::Left, Finger::Index, 1, 1, 3, Some(KeyUsage::F), "F"),
    slot(Hand::Left, Finger::Index, 2, 2, 3, Some(KeyUsage::V), "V"),
    slot(Hand::Left, Finger::Index, 3, 0, 4, Some(KeyUsage::T), "T"),
    slot(Hand::Left, Finger::Index, 4, 1, 4, Some(KeyUsage::G), "G"),
    slot(Hand::Left, Finger::Index, 5, 2, 4, Some(KeyUsage::B), "B"),
    slot(Hand::Left, Finger::Thumb, 0, 3, 3, None, "left-outer-thumb"),
    slot(
        Hand::Left,
        Finger::Thumb,
        1,
        3,
        4,
        Some(KeyUsage::Backspace),
        "Backspace",
    ),
    slot(Hand::Right, Finger::Index, 0, 0, 7, Some(KeyUsage::U), "U"),
    slot(Hand::Right, Finger::Index, 1, 1, 7, Some(KeyUsage::J), "J"),
    slot(Hand::Right, Finger::Index, 2, 2, 7, Some(KeyUsage::M), "M"),
    slot(Hand::Right, Finger::Index, 3, 0, 6, Some(KeyUsage::Y), "Y"),
    slot(Hand::Right, Finger::Index, 4, 1, 6, Some(KeyUsage::H), "H"),
    slot(Hand::Right, Finger::Index, 5, 2, 6, Some(KeyUsage::N), "N"),
    slot(Hand::Right, Finger::Middle, 0, 0, 8, Some(KeyUsage::I), "I"),
    slot(Hand::Right, Finger::Middle, 1, 1, 8, Some(KeyUsage::K), "K"),
    slot(Hand::Right, Finger::Middle, 2, 2, 8, None, "comma"),
    slot(Hand::Right, Finger::Ring, 0, 0, 9, Some(KeyUsage::O), "O"),
    slot(Hand::Right, Finger::Ring, 1, 1, 9, Some(KeyUsage::L), "L"),
    slot(Hand::Right, Finger::Ring, 2, 2, 9, None, "period"),
    slot(Hand::Right, Finger::Pinky, 0, 0, 10, Some(KeyUsage::P), "P"),
    slot(Hand::Right, Finger::Pinky, 1, 1, 10, None, "semicolon"),
    slot(Hand::Right, Finger::Pinky, 2, 2, 10, None, "slash"),
    slot(
        Hand::Right,
        Finger::Thumb,
        0,
        3,
        7,
        None,
        "right-outer-thumb",
    ),
    slot(
        Hand::Right,
        Finger::Thumb,
        1,
        3,
        6,
        Some(KeyUsage::Space),
        "Space",
    ),
];

/// Return the complete physical layout in stable manifest order.
#[must_use]
pub const fn all_slots() -> &'static [KeySlot; PHYSICAL_POSITION_COUNT] {
    &ALL_SLOTS
}

/// Render the exact Core-28-on-34 layout contract as canonical ASCII bytes.
///
/// Records stay in [`all_slots`] order. Display coordinates preserve the
/// separate visual order without requiring a second key table in adapters.
#[must_use]
pub fn core28_layout_manifest_bytes() -> Vec<u8> {
    let mut manifest = String::with_capacity(2_048);
    writeln!(manifest, "schema|{CORE28_LAYOUT_MANIFEST_SCHEMA}")
        .expect("writing to a String cannot fail");
    writeln!(manifest, "layout-id|{CORE28_LAYOUT_ID}").expect("writing to a String cannot fail");
    manifest.push_str("ordinal|row|column|hand|finger|slot|label|state|output\n");

    for (ordinal, slot) in ALL_SLOTS.iter().enumerate() {
        let (state, output) = slot
            .usage
            .map_or(("reserved", "none"), |_| ("enabled", slot.label));
        writeln!(
            manifest,
            "{ordinal:02}|{}|{}|{}|{}|{}|{}|{state}|{output}",
            slot.display_row,
            slot.display_column,
            hand_name(slot.position.hand),
            finger_name(slot.position.finger),
            slot.position.slot,
            slot.label,
        )
        .expect("writing to a String cannot fail");
    }

    manifest.into_bytes()
}

const fn hand_name(hand: Hand) -> &'static str {
    match hand {
        Hand::Left => "left",
        Hand::Right => "right",
    }
}

const fn finger_name(finger: Finger) -> &'static str {
    match finger {
        Finger::Thumb => "thumb",
        Finger::Index => "index",
        Finger::Middle => "middle",
        Finger::Ring => "ring",
        Finger::Pinky => "pinky",
    }
}

/// Resolve one complete slot from its physical position.
#[must_use]
pub fn slot_for_position(position: PhysicalPosition) -> Option<&'static KeySlot> {
    ALL_SLOTS.iter().find(|slot| slot.position == position)
}

/// Resolve one complete slot from its stable manifest label.
#[must_use]
pub fn slot_for_label(label: &str) -> Option<&'static KeySlot> {
    ALL_SLOTS.iter().find(|slot| slot.label == label)
}

/// Resolve an enabled usage from a physical position.
#[must_use]
pub fn usage_for_position(position: PhysicalPosition) -> Option<KeyUsage> {
    slot_for_position(position).and_then(|slot| slot.usage)
}

/// Resolve the unique physical position for an enabled Core-28 usage.
#[must_use]
pub fn position_for_usage(usage: KeyUsage) -> Option<PhysicalPosition> {
    ALL_SLOTS
        .iter()
        .find(|slot| slot.usage == Some(usage))
        .map(|slot| slot.position)
}

/// Resolve the unique physical position for a stable manifest label.
#[must_use]
pub fn position_for_label(label: &str) -> Option<PhysicalPosition> {
    slot_for_label(label).map(|slot| slot.position)
}

/// Return a physical position's stable index in the 34-position manifest.
#[must_use]
pub fn position_index(position: PhysicalPosition) -> Option<usize> {
    ALL_SLOTS.iter().position(|slot| slot.position == position)
}

/// Report whether a regular keyboard press is valid warm-start evidence.
///
/// B, Space, and Backspace use deliberate Core-28 finger ownership that a
/// normal keyboard cannot guarantee. The two outer-thumb positions have no
/// key. All five positions are therefore learned only on the passive surface.
#[must_use]
pub const fn keyboard_warm_start_eligible(position: PhysicalPosition) -> bool {
    !matches!(
        (position.hand, position.finger, position.slot),
        (Hand::Left | Hand::Right, Finger::Thumb, 0)
            | (Hand::Left, Finger::Thumb, 1)
            | (Hand::Right, Finger::Thumb, 1)
            | (Hand::Left, Finger::Index, 5)
    )
}
