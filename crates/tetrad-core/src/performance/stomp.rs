//! Stomp footswitch assignments, labels and momentary flags.
//!
//! Evidence: eleven footswitches (momentary setter `0x5227c4` rejects unsigned
//! index > 10), Gig rename slot `0x65cb3c`, label setter `0x514474`, name
//! helper `0x2c4938`, auto-assign `0x510d70`, load merge `0x681d44`. Preset
//! fields: `legacy_stomp_mode_stomp_data` (13), `stomp_mode_assignments` (27),
//! `stomp_labels` (32), `single_stomp_labels` (34), `stomp_is_momentary` (35).

use std::collections::BTreeMap;

use crate::workflow::UiText;

pub const STOMPS: u8 = 11;
/// Assigned model that forces momentary off (`0x514598`).
pub const NON_MOMENTARY_MODEL: u32 = 27000;
/// Model whose parameter 1 mirrors the momentary flag as 1.0/0.0 (`0x522970`).
pub const MOMENTARY_MIRROR_MODEL: u32 = 26001;

/// Logical stomp index: the key of assignments, labels and momentary flags.
/// Letters A–H are 0–7 (see `super::footswitch` for HAL indices).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StompIndex(u8);

impl StompIndex {
    pub fn new(index: u32) -> Option<Self> {
        (index < u32::from(STOMPS)).then_some(Self(index as u8))
    }

    pub fn get(self) -> u8 {
        self.0
    }
}

/// `StompModeAssignment.Type`; an absent field is the proto3 default PRIMARY.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum AssignmentType {
    #[default]
    Primary,
    Secondary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StompAssignment {
    pub row: u8,
    pub column: u8,
    pub stomp: StompIndex,
    pub kind: AssignmentType,
}

/// Auto-assign of a new block when "Stomp Mode Bypass Assignment" is on
/// (`0x510d70`): the first stomp 0..=7 without assignments, as PRIMARY.
pub fn auto_assign_stomp(assignments: &[StompAssignment]) -> Option<StompIndex> {
    (0..8)
        .map(StompIndex)
        .find(|stomp| assignments.iter().all(|a| a.stomp != *stomp))
}

/// One stored `StompModeAssignment` before validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawAssignment {
    pub row: u32,
    pub column: u32,
    pub stomp: u32,
    /// Absent is PRIMARY; any nonzero value acts as SECONDARY on press.
    pub kind: Option<u32>,
}

/// Load merge of `0x681d44`, keyed by (row, column, type): legacy entry `i`
/// with row < 4 and column < 8 becomes PRIMARY stomp `i`; modern entries then
/// override. Stock logs and skips modern stomps ≥ 11. Owned policy also skips
/// legacy positions ≥ 11 and modern blocks outside the 4×8 grid. Returns the
/// assignments in key order and the number skipped.
pub fn merge_assignments(
    legacy: &[(u32, u32)],
    modern: &[RawAssignment],
) -> (Vec<StompAssignment>, usize) {
    let mut merged = BTreeMap::new();
    let mut skipped = 0;
    let in_grid = |row: u32, column: u32| row < 4 && column < 8;
    for (position, &(row, column)) in legacy.iter().enumerate() {
        if !in_grid(row, column) {
            continue;
        }
        match u32::try_from(position).ok().and_then(StompIndex::new) {
            Some(stomp) => {
                merged.insert((row as u8, column as u8, AssignmentType::Primary), stomp);
            }
            None => skipped += 1,
        }
    }
    for raw in modern {
        let kind = if raw.kind.unwrap_or(0) == 0 {
            AssignmentType::Primary
        } else {
            AssignmentType::Secondary
        };
        match StompIndex::new(raw.stomp).filter(|_| in_grid(raw.row, raw.column)) {
            Some(stomp) => {
                merged.insert((raw.row as u8, raw.column as u8, kind), stomp);
            }
            None => skipped += 1,
        }
    }
    let assignments = merged
        .into_iter()
        .map(|((row, column, kind), stomp)| StompAssignment {
            row,
            column,
            stomp,
            kind,
        })
        .collect();
    (assignments, skipped)
}

/// Which preset map holds a stomp label: grouped (field 32) when more than one
/// block is assigned to the stomp, else single (field 34).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabelMap {
    Grouped,
    Single,
}

impl LabelMap {
    pub fn for_assignment_count(count: usize) -> Self {
        if count > 1 {
            Self::Grouped
        } else {
            Self::Single
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LabelEdit {
    /// Empty text removes the map key.
    Clear,
    Set(UiText),
    Unchanged,
}

/// Rename rule of `0x65cb3c`: the generated default label clears the stored
/// label; any other text is stored only when it differs from the old label.
pub fn label_edit(submitted: &UiText, generated_default: &UiText, old: &UiText) -> LabelEdit {
    if submitted == generated_default {
        LabelEdit::Clear
    } else if submitted != old {
        LabelEdit::Set(submitted.clone())
    } else {
        LabelEdit::Unchanged
    }
}

/// Rename-dialog name helper `0x2c4938`: Qt `simplified()`, then uppercase the
/// first UTF-16 unit when it is at most 255 and `islower` accepts it. The C
/// locale is assumed (inferred): only `a..=z` qualify.
pub fn stomp_name(input: UiText) -> UiText {
    let simplified = input.simplified_pinned_qt48();
    let mut units = simplified.units().to_vec();
    if let Some(first) = units.first_mut()
        && (u16::from(b'a')..=u16::from(b'z')).contains(first)
    {
        *first -= 0x20;
    }
    UiText::from_units(units).expect("never longer than the simplified input")
}

/// Effective momentary flag of the setter: requested AND NOT an assigned 27000.
/// Stock returns without change on Atma; the caller applies that platform gate.
pub fn effective_momentary(requested: bool, assigned_non_momentary_model: bool) -> bool {
    requested && !assigned_non_momentary_model
}

/// The dirty timer starts on the request, even when momentary is refused.
pub fn rename_starts_dirty(
    submitted: &UiText,
    old_label: &UiText,
    requested_momentary: bool,
    current_momentary: bool,
) -> bool {
    submitted != old_label || requested_momentary != current_momentary
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(s: &str) -> UiText {
        UiText::from_literal(s).unwrap()
    }

    #[test]
    fn stomp_indices_labels_and_names_follow_recovered_rules() {
        assert_eq!(StompIndex::new(10).map(StompIndex::get), Some(10));
        assert_eq!(StompIndex::new(11), None);
        assert_eq!(LabelMap::for_assignment_count(2), LabelMap::Grouped);
        assert_eq!(LabelMap::for_assignment_count(1), LabelMap::Single);
        let default = text("DRIVE");
        assert_eq!(label_edit(&default, &default, &text("X")), LabelEdit::Clear);
        assert_eq!(
            label_edit(&text("X"), &default, &text("X")),
            LabelEdit::Unchanged
        );
        assert_eq!(
            label_edit(&text("Y"), &default, &text("X")),
            LabelEdit::Set(text("Y"))
        );
        assert_eq!(stomp_name(text("  big   muff ")), text("Big muff"));
        assert_eq!(stomp_name(text("écho")), text("écho"));
        assert_eq!(stomp_name(text("   ")), text(""));
        assert!(!effective_momentary(true, true));
        assert!(rename_starts_dirty(&text("A"), &text("A"), true, false));
    }

    #[test]
    fn auto_assign_and_load_merge_follow_the_loader() {
        let stomp = |i| StompIndex::new(i).unwrap();
        let taken: Vec<_> = (0..3)
            .map(|i| StompAssignment {
                row: 0,
                column: i as u8,
                stomp: stomp(i),
                kind: AssignmentType::Primary,
            })
            .collect();
        assert_eq!(auto_assign_stomp(&taken), Some(stomp(3)));
        let full: Vec<_> = (0..8)
            .map(|i| StompAssignment {
                stomp: stomp(i),
                ..taken[0]
            })
            .collect();
        assert_eq!(auto_assign_stomp(&full), None);
        let modern = [
            RawAssignment {
                row: 1,
                column: 2,
                stomp: 6,
                kind: None,
            },
            RawAssignment {
                row: 1,
                column: 2,
                stomp: 4,
                kind: Some(1),
            },
            RawAssignment {
                row: 0,
                column: 0,
                stomp: 11,
                kind: None,
            },
        ];
        let (merged, skipped) = merge_assignments(&[(1, 2), (9, 0), (0, 0)], &modern);
        assert_eq!(skipped, 1);
        let keys: Vec<_> = merged
            .iter()
            .map(|a| (a.row, a.column, a.kind, a.stomp.get()))
            .collect();
        assert_eq!(
            keys,
            [
                (0, 0, AssignmentType::Primary, 2),
                (1, 2, AssignmentType::Primary, 6),
                (1, 2, AssignmentType::Secondary, 4),
            ]
        );
    }
}
