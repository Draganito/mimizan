//! The editable development of one negative and its history. An `Edit` is
//! the paper's "Entwicklungsdatensatz": the recipe the export renders plus
//! the two things that are not in the recipe because they are the
//! negative's, not the print's (the channel weights and the user's turns).
//! The history is a list of such states, never of pixels: undo and redo
//! swap the whole state, and every state is one export.

use mimizan_core::mix::Weights;
use mimizan_core::plate::PlateParams;
use mimizan_core::recipe::Recipe;

#[derive(Clone, Debug, PartialEq)]
pub struct Edit {
    pub weights: Weights,
    pub recipe: Recipe,
    /// Clockwise quarter turns added to the file's orientation (0..3).
    pub rotation: u8,
    /// The three-colour plate: taking filters and projection primaries.
    /// `None` is the monochrome negative. Not in the recipe, like the
    /// weights: it is the negative's, not the print's.
    pub plate: Option<PlateParams>,
    /// The red and the blue lamp of the projector, stops added to those
    /// two positives. Only read while `plate` is on.
    pub lamps: [f64; 2],
}

impl Edit {
    /// The untouched development: the paper at its base exposure.
    pub fn new(weights: Weights) -> Self {
        Self { weights, recipe: Recipe::on_paper(), rotation: 0, plate: None, lamps: [0.0; 2] }
    }

    /// The three lamps the print takes: red, green (fixed), blue.
    pub fn lamps_rgb(&self) -> [f64; 3] {
        [self.lamps[0], 0.0, self.lamps[1]]
    }
}

/// Linear history with a cursor. `commit` is cheap to call every frame: it
/// records only when the state differs from the current entry.
pub struct History {
    entries: Vec<Edit>,
    cursor: usize,
}

/// Oldest states fall off once the list is this long.
const MAX: usize = 200;

impl History {
    pub fn new(first: Edit) -> Self {
        Self { entries: vec![first], cursor: 0 }
    }

    pub fn current(&self) -> &Edit {
        &self.entries[self.cursor]
    }

    /// Record `e` as the newest state when it differs from the current one.
    /// Anything after the cursor (undone states) is dropped. Returns whether
    /// a new entry was made.
    pub fn commit(&mut self, e: &Edit) -> bool {
        if self.current() == e {
            return false;
        }
        self.entries.truncate(self.cursor + 1);
        self.entries.push(e.clone());
        if self.entries.len() > MAX {
            let extra = self.entries.len() - MAX;
            self.entries.drain(0..extra);
        }
        self.cursor = self.entries.len() - 1;
        true
    }

    pub fn can_undo(&self) -> bool {
        self.cursor > 0
    }

    pub fn can_redo(&self) -> bool {
        self.cursor + 1 < self.entries.len()
    }

    pub fn undo(&mut self) -> Option<&Edit> {
        if !self.can_undo() {
            return None;
        }
        self.cursor -= 1;
        Some(self.current())
    }

    pub fn redo(&mut self) -> Option<&Edit> {
        if !self.can_redo() {
            return None;
        }
        self.cursor += 1;
        Some(self.current())
    }

    /// (position, length) for the status line: "step 3 of 7".
    pub fn position(&self) -> (usize, usize) {
        (self.cursor + 1, self.entries.len())
    }

    /// Write a measured radius and amount into every step. The checkbox is
    /// left as each step had it, and this is not a new step of its own.
    pub fn fill_peak(&mut self, radius: f64, amount: f64) {
        for e in &mut self.entries {
            e.recipe.peak.radius = radius;
            e.recipe.peak.amount = amount;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_place(p: f64) -> Edit {
        let mut e = Edit::new(Weights::default());
        e.recipe.place = p;
        e
    }

    #[test]
    fn commit_records_only_changes_and_drops_the_redo_tail() {
        let mut h = History::new(with_place(0.0));
        assert!(!h.commit(&with_place(0.0)));
        assert!(h.commit(&with_place(0.5)));
        assert!(h.commit(&with_place(1.0)));
        assert_eq!(h.position(), (3, 3));
        assert_eq!(h.undo().unwrap().recipe.place, 0.5);
        assert_eq!(h.undo().unwrap().recipe.place, 0.0);
        assert!(h.undo().is_none());
        assert_eq!(h.redo().unwrap().recipe.place, 0.5);
        // A new state after an undo forgets the undone one.
        assert!(h.commit(&with_place(-1.0)));
        assert!(!h.can_redo());
        assert_eq!(h.position(), (3, 3));
        assert_eq!(h.undo().unwrap().recipe.place, 0.5);
        assert_eq!(h.undo().unwrap().recipe.place, 0.0);
    }

    #[test]
    fn history_is_bounded() {
        let mut h = History::new(with_place(0.0));
        for i in 1..(MAX + 50) {
            h.commit(&with_place(i as f64 * 1e-3));
        }
        assert_eq!(h.position().1, MAX);
        assert_eq!(h.current().recipe.place, (MAX + 49) as f64 * 1e-3);
    }
}
