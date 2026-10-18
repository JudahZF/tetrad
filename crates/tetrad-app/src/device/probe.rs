//! The costliest unit of each phase of one [`super::Device::step`] (A3). A bring-up `SLOW` pass
//! line names it, so one QC run tells what costs: the Control message in `reduce`, the effect kind
//! in `apply`, the view step in `views`. Diagnostics only; no logic reads it.
//!
//! A unit's time is its own time: the time of the units inside it (an effect that runs more
//! effects, a view step that applies effects) counts for those units only. Units of the same
//! phase and label add up, so 300 small `ControlReport` effects show as one unit with a count.

use std::cell::{Cell, RefCell};
use std::fmt::{self, Write};
use std::time::{Duration, Instant};

/// The [`super::PhaseTimes`] phases.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Phase {
    #[default]
    Gather,
    Reduce,
    Apply,
    Control,
    Views,
    Frame,
}

impl Phase {
    pub const ALL: [Self; 6] = [
        Self::Gather,
        Self::Reduce,
        Self::Apply,
        Self::Control,
        Self::Views,
        Self::Frame,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::Gather => "gather",
            Self::Reduce => "reduce",
            Self::Apply => "apply",
            Self::Control => "control",
            Self::Views => "views",
            Self::Frame => "frame",
        }
    }
}

const LABEL_BYTES: usize = 64;

/// A unit name in a fixed buffer: naming a unit allocates nothing. Longer text is cut.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Label {
    bytes: [u8; LABEL_BYTES],
    len: u8,
}

impl Label {
    pub fn new(text: impl fmt::Display) -> Self {
        let mut label = Self::empty();
        // A full label stops the formatting with an error; the cut text stays.
        let _ = write!(label, "{text}");
        label
    }

    /// The variant path of a derived `Debug` value, at most two names deep: `Deploy(Update {
    /// .. })` gives `Deploy Update`, `ControlReport([..])` gives `ControlReport`. The formatting
    /// stops at the first character that is not part of a name, so a large payload is not
    /// formatted.
    pub fn of(value: &impl fmt::Debug) -> Self {
        let mut head = Head {
            label: Self::empty(),
            open: false,
        };
        let _ = write!(head, "{value:?}");
        head.label
    }

    fn empty() -> Self {
        Self {
            bytes: [0; LABEL_BYTES],
            len: 0,
        }
    }

    pub fn as_str(&self) -> &str {
        // Only whole chars are pushed (`write_str`).
        std::str::from_utf8(&self.bytes[..usize::from(self.len)])
            .unwrap_or_default()
            .trim_end()
    }
}

impl Write for Label {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        for ch in text.chars() {
            let start = usize::from(self.len);
            let end = start + ch.len_utf8();
            if end > LABEL_BYTES {
                return Err(fmt::Error);
            }
            ch.encode_utf8(&mut self.bytes[start..end]);
            self.len = end as u8;
        }
        Ok(())
    }
}

impl fmt::Debug for Label {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// [`Label::of`]: keeps names and the first `(`, as a space.
struct Head {
    label: Label,
    open: bool,
}

impl Write for Head {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        for ch in text.chars() {
            match ch {
                ch if ch.is_alphanumeric() || ch == '_' => {
                    self.label.write_char(ch)?;
                }
                '(' if !self.open => {
                    self.open = true;
                    self.label.write_char(' ')?;
                }
                _ => return Err(fmt::Error),
            }
        }
        Ok(())
    }
}

/// The own time of one label in one phase, over `count` runs.
#[derive(Debug, Clone, Copy)]
pub struct Unit {
    pub phase: Phase,
    pub label: Label,
    pub time: Duration,
    pub count: u32,
}

impl fmt::Display for Unit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.label.as_str())?;
        if self.count > 1 {
            write!(f, " x{}", self.count)?;
        }
        match self.time.as_millis() {
            0 => write!(f, " {}us", self.time.as_micros()),
            ms => write!(f, " {ms}ms"),
        }
    }
}

/// The units of the pass in progress. Shared access (`&self`), so a `&self` device method can
/// mark a unit too.
#[derive(Debug, Default)]
pub struct Probe {
    phase: Cell<Phase>,
    units: RefCell<Vec<Unit>>,
    /// Open units: their start and the time of the units inside them.
    open: RefCell<Vec<(Instant, Duration)>>,
}

impl Probe {
    /// A new pass (a panic in the last one may have left units open).
    pub fn reset(&self) {
        self.phase.set(Phase::Gather);
        self.units.borrow_mut().clear();
        self.open.borrow_mut().clear();
    }

    pub fn set_phase(&self, phase: Phase) {
        self.phase.set(phase);
    }

    /// Opens a unit; [`Self::leave`] closes the newest one.
    pub fn enter(&self) {
        self.open
            .borrow_mut()
            .push((Instant::now(), Duration::ZERO));
    }

    pub fn leave(&self, label: Label) {
        let mut open = self.open.borrow_mut();
        let Some((start, inner)) = open.pop() else {
            return;
        };
        let total = start.elapsed();
        if let Some((_, parent)) = open.last_mut() {
            *parent += total;
        }
        let (phase, time) = (self.phase.get(), total.saturating_sub(inner));
        let mut units = self.units.borrow_mut();
        match (units.iter_mut()).find(|unit| unit.phase == phase && unit.label == label) {
            Some(unit) => {
                unit.time += time;
                unit.count += 1;
            }
            None => units.push(Unit {
                phase,
                label,
                time,
                count: 1,
            }),
        }
    }

    /// The costliest unit of `phase` in the last pass.
    pub fn top(&self, phase: Phase) -> Option<Unit> {
        (self.units.borrow().iter())
            .filter(|unit| unit.phase == phase)
            .max_by_key(|unit| unit.time)
            .copied()
    }

    /// The costliest unit of each phase that took at least `floor`, for the `SLOW` line:
    /// `reduce: Control File Read 141ms; apply: ControlReport x290 120ms`.
    pub fn summary(&self, floor: Duration) -> String {
        let mut out = String::new();
        for phase in Phase::ALL {
            if let Some(unit) = self.top(phase).filter(|unit| unit.time >= floor) {
                let gap = if out.is_empty() { "" } else { "; " };
                let _ = write!(out, "{gap}{}: {unit}", phase.name());
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    #[allow(dead_code)]
    enum Effect {
        Present,
        Deploy(Update),
        Report(Box<[u8; 4]>),
        Notice(u8, String),
        StartTimer { after: u32 },
    }

    #[derive(Debug)]
    #[allow(dead_code)]
    enum Update {
        Layout { blocks: Vec<u32> },
    }

    /// A label names at most two variants and never formats the payload.
    #[test]
    fn a_label_is_the_variant_path_of_the_debug_text() {
        let of = |effect: Effect| Label::of(&effect).as_str().to_owned();
        assert_eq!(of(Effect::Present), "Present");
        let layout = Update::Layout { blocks: vec![1; 9] };
        assert_eq!(of(Effect::Deploy(layout)), "Deploy Layout");
        assert_eq!(of(Effect::Report(Box::new([0; 4]))), "Report");
        assert_eq!(of(Effect::Notice(3, "text".into())), "Notice 3");
        assert_eq!(of(Effect::StartTimer { after: 1 }), "StartTimer");
        let long = "x".repeat(100);
        assert_eq!(Label::new(&long).as_str().len(), LABEL_BYTES);
    }

    /// Own time: a unit inside another counts once, for itself; same labels add up.
    #[test]
    fn nested_units_count_their_own_time() {
        let probe = Probe::default();
        probe.set_phase(Phase::Apply);
        probe.enter();
        for _ in 0..2 {
            probe.enter();
            std::thread::sleep(Duration::from_millis(5));
            probe.leave(Label::new("Inner"));
        }
        probe.leave(Label::new("Outer"));
        let top = probe.top(Phase::Apply).unwrap();
        assert_eq!((top.label.as_str(), top.count), ("Inner", 2));
        assert!(top.time >= Duration::from_millis(10));
        let units = probe.units.borrow();
        let outer = (units.iter()).find(|u| u.label.as_str() == "Outer");
        assert!(outer.unwrap().time < Duration::from_millis(5));
        assert!(
            probe
                .summary(Duration::ZERO)
                .starts_with("apply: Inner x2 ")
        );
        assert!(probe.top(Phase::Reduce).is_none());
    }
}
