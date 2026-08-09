use std::collections::{BTreeSet, HashSet};

use invisible_keyboard::layout::{
    CORE28_KEY_COUNT, CORE28_LAYOUT_ID, CORE28_LAYOUT_MANIFEST_SCHEMA, Finger, Hand, KeyUsage,
    PHYSICAL_POSITION_COUNT, RESERVED_POSITION_COUNT, all_slots, core28_layout_manifest_bytes,
    keyboard_warm_start_eligible, position_for_usage, slot_for_label, slot_for_position,
    usage_for_position,
};
use invisible_keyboard::manifest::{format_sha256, sha256};

const EXPECTED_CORE28_LAYOUT_MANIFEST: &str = concat!(
    "schema|invisible-keyboard-layout-manifest-v1\n",
    "layout-id|invisible-keyboard-core28-v1\n",
    "ordinal|row|column|hand|finger|slot|label|state|output\n",
    "00|0|0|left|pinky|0|Q|enabled|Q\n",
    "01|1|0|left|pinky|1|A|enabled|A\n",
    "02|2|0|left|pinky|2|Z|enabled|Z\n",
    "03|0|1|left|ring|0|W|enabled|W\n",
    "04|1|1|left|ring|1|S|enabled|S\n",
    "05|2|1|left|ring|2|X|enabled|X\n",
    "06|0|2|left|middle|0|E|enabled|E\n",
    "07|1|2|left|middle|1|D|enabled|D\n",
    "08|2|2|left|middle|2|C|enabled|C\n",
    "09|0|3|left|index|0|R|enabled|R\n",
    "10|1|3|left|index|1|F|enabled|F\n",
    "11|2|3|left|index|2|V|enabled|V\n",
    "12|0|4|left|index|3|T|enabled|T\n",
    "13|1|4|left|index|4|G|enabled|G\n",
    "14|2|4|left|index|5|B|enabled|B\n",
    "15|3|3|left|thumb|0|left-outer-thumb|reserved|none\n",
    "16|3|4|left|thumb|1|Backspace|enabled|Backspace\n",
    "17|0|7|right|index|0|U|enabled|U\n",
    "18|1|7|right|index|1|J|enabled|J\n",
    "19|2|7|right|index|2|M|enabled|M\n",
    "20|0|6|right|index|3|Y|enabled|Y\n",
    "21|1|6|right|index|4|H|enabled|H\n",
    "22|2|6|right|index|5|N|enabled|N\n",
    "23|0|8|right|middle|0|I|enabled|I\n",
    "24|1|8|right|middle|1|K|enabled|K\n",
    "25|2|8|right|middle|2|comma|reserved|none\n",
    "26|0|9|right|ring|0|O|enabled|O\n",
    "27|1|9|right|ring|1|L|enabled|L\n",
    "28|2|9|right|ring|2|period|reserved|none\n",
    "29|0|10|right|pinky|0|P|enabled|P\n",
    "30|1|10|right|pinky|1|semicolon|reserved|none\n",
    "31|2|10|right|pinky|2|slash|reserved|none\n",
    "32|3|7|right|thumb|0|right-outer-thumb|reserved|none\n",
    "33|3|6|right|thumb|1|Space|enabled|Space\n",
);

const EXPECTED_CORE28_LAYOUT_SHA256: &str =
    "a12422c46b738850bdf2535f5a8993f1e7867b63357f5ab68dada804153902a5";

#[test]
fn physical_layout_has_34_unique_positions() {
    let slots = all_slots();
    let positions = slots
        .iter()
        .map(|slot| slot.position)
        .collect::<HashSet<_>>();

    assert_eq!(slots.len(), PHYSICAL_POSITION_COUNT);
    assert_eq!(positions.len(), PHYSICAL_POSITION_COUNT);
    assert_eq!(
        slots
            .iter()
            .map(|slot| (slot.display_row, slot.display_column))
            .collect::<HashSet<_>>()
            .len(),
        PHYSICAL_POSITION_COUNT
    );
    assert_eq!(
        slots.iter().filter(|slot| slot.usage.is_none()).count(),
        RESERVED_POSITION_COUNT
    );
}

#[test]
fn canonical_manifest_freezes_exact_core28_on_34_mapping() {
    let manifest = core28_layout_manifest_bytes();

    assert_eq!(
        CORE28_LAYOUT_MANIFEST_SCHEMA,
        "invisible-keyboard-layout-manifest-v1"
    );
    assert_eq!(CORE28_LAYOUT_ID, "invisible-keyboard-core28-v1");
    assert_eq!(manifest, EXPECTED_CORE28_LAYOUT_MANIFEST.as_bytes());
    assert_eq!(
        format_sha256(&sha256(&manifest)),
        EXPECTED_CORE28_LAYOUT_SHA256
    );
    assert!(manifest.is_ascii());
    assert_eq!(manifest.iter().filter(|byte| **byte == b'\n').count(), 37);
}

#[test]
fn core28_contains_every_letter_once_plus_space_and_backspace() {
    let usages = all_slots()
        .iter()
        .filter_map(|slot| slot.usage)
        .collect::<Vec<_>>();
    let unique = usages.iter().copied().collect::<HashSet<_>>();

    assert_eq!(usages.len(), CORE28_KEY_COUNT);
    assert_eq!(unique.len(), CORE28_KEY_COUNT);

    for letter in 'A'..='Z' {
        let usage = KeyUsage::from_ascii_letter(letter).expect("A-Z is supported");
        assert_eq!(
            usages
                .iter()
                .filter(|candidate| **candidate == usage)
                .count(),
            1
        );
    }

    assert_eq!(
        usages
            .iter()
            .filter(|usage| **usage == KeyUsage::Space)
            .count(),
        1
    );
    assert_eq!(
        usages
            .iter()
            .filter(|usage| **usage == KeyUsage::Backspace)
            .count(),
        1
    );
}

#[test]
fn mapping_is_bidirectional_for_enabled_positions() {
    for slot in all_slots() {
        assert_eq!(slot_for_position(slot.position), Some(slot));
        assert_eq!(slot_for_label(slot.label), Some(slot));
        assert_eq!(usage_for_position(slot.position), slot.usage);

        if let Some(usage) = slot.usage {
            assert_eq!(position_for_usage(usage), Some(slot.position));
        }
    }
}

#[test]
fn unknown_positions_and_labels_do_not_resolve() {
    assert_eq!(slot_for_label("not-a-layout-position"), None);
    assert_eq!(
        slot_for_position(invisible_keyboard::layout::PhysicalPosition {
            hand: Hand::Left,
            finger: invisible_keyboard::layout::Finger::Thumb,
            slot: u8::MAX,
        }),
        None
    );
}

#[test]
fn reserved_positions_are_exact_and_explicit() {
    let reserved = all_slots()
        .iter()
        .filter(|slot| slot.usage.is_none())
        .map(|slot| {
            (
                slot.label,
                slot.position.hand,
                slot.position.finger,
                slot.position.slot,
                slot.display_row,
                slot.display_column,
            )
        })
        .collect::<Vec<_>>();

    assert_eq!(
        reserved,
        vec![
            ("left-outer-thumb", Hand::Left, Finger::Thumb, 0, 3, 3),
            ("comma", Hand::Right, Finger::Middle, 2, 2, 8),
            ("period", Hand::Right, Finger::Ring, 2, 2, 9),
            ("semicolon", Hand::Right, Finger::Pinky, 1, 1, 10),
            ("slash", Hand::Right, Finger::Pinky, 2, 2, 10),
            ("right-outer-thumb", Hand::Right, Finger::Thumb, 0, 3, 7),
        ]
    );
}

#[test]
fn unsupported_ascii_characters_do_not_enter_core28() {
    for character in [';', ',', '.', '/', '0', '\n', '\t'] {
        assert_eq!(KeyUsage::from_ascii_letter(character), None);
    }
}

#[test]
fn keyboard_warm_start_excludes_ambiguous_and_passive_only_positions() {
    let excluded = all_slots()
        .iter()
        .filter(|slot| !keyboard_warm_start_eligible(slot.position))
        .collect::<Vec<_>>();
    let excluded_labels = excluded
        .iter()
        .map(|slot| slot.label)
        .collect::<BTreeSet<_>>();

    assert_eq!(
        excluded_labels,
        BTreeSet::from([
            "B",
            "Backspace",
            "Space",
            "left-outer-thumb",
            "right-outer-thumb",
        ])
    );
    for usage in [KeyUsage::B, KeyUsage::Space, KeyUsage::Backspace] {
        let position = position_for_usage(usage).expect("Core-28 usage has a position");
        assert!(!keyboard_warm_start_eligible(position));
    }
    assert_eq!(
        excluded
            .iter()
            .filter(|slot| slot.position.finger == invisible_keyboard::layout::Finger::Thumb)
            .count(),
        4
    );
}
