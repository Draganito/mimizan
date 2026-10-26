//! The editable development of one negative and its history. An `Edit` is
//! the paper's "Entwicklungsdatensatz": the recipe the export renders plus
//! what the export takes beside it (the mix, the plate and its lamps, the
//! user's turns). The file next to the RAW carries the mix and the plate
//! inside the recipe (`recipe_for_file`); in memory they are fields of
//! their own and the recipe's slots stay empty. The history is a list of
//! such states, never of pixels: undo and redo swap the whole state, and
//! every state is one export.

use mimizan_core::mix::Weights;
use mimizan_core::plate::PlateParams;
use mimizan_core::recipe::{PlateRecipe, Recipe};

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

    /// The recipe as the file next to the RAW holds it: the print, plus
    /// the mix and the plate this development runs under. `mix` is
    /// written only for a colour sensor (`bayer`); a monochrome sensor
    /// has no mix.
    pub fn recipe_for_file(&self, bayer: bool) -> Recipe {
        let mut r = self.recipe.clone();
        r.mix = bayer.then_some(self.weights);
        r.plate = self.plate.filter(|_| bayer).map(|p| PlateRecipe {
            filters: p.filters,
            space: p.space,
            lamps: self.lamps,
        });
        r
    }

    /// Take a loaded recipe apart: the print stays in `recipe`, the mix
    /// and the plate go to their fields. A recipe without a mix keeps
    /// the weights the edit had; one without a plate is the monochrome
    /// process.
    pub fn take_recipe(&mut self, mut r: Recipe) {
        if let Some(m) = r.mix.take() {
            self.weights = m;
        }
        match r.plate.take() {
            Some(p) => {
                self.plate = Some(p.params());
                self.lamps = p.lamps;
            }
            None => {
                self.plate = None;
                self.lamps = [0.0; 2];
            }
        }
        self.recipe = r;
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
    fn the_file_carries_mix_and_plate_and_comes_back_whole() {
        use mimizan_core::plate::{PlateFilters, PlateSpace};
        let mut e = Edit::new(Weights { r: 0.3, g: 0.5, b: 0.2 });
        e.plate = Some(PlateParams { filters: PlateFilters::Wratten, space: PlateSpace::Srgb });
        e.lamps = [0.2, -0.1];
        e.recipe.place = -0.5;
        let r = e.recipe_for_file(true);
        assert_eq!(r.mix, Some(e.weights));
        assert_eq!(
            r.plate.map(|p| (p.filters, p.space, p.lamps)),
            Some((PlateFilters::Wratten, PlateSpace::Srgb, [0.2, -0.1]))
        );
        assert_eq!(r.place, -0.5);
        // In memory the slots stay empty.
        assert!(e.recipe.mix.is_none() && e.recipe.plate.is_none());
        // Back into a fresh edit: everything returns, the slots stay empty.
        let mut back = Edit::new(Weights::default());
        back.take_recipe(r);
        assert_eq!(back, e);
        // A monochrome sensor writes neither.
        let m = e.recipe_for_file(false);
        assert!(m.mix.is_none() && m.plate.is_none());
        // A recipe without a plate is the monochrome process, lamps off.
        let mut mono = e.clone();
        mono.take_recipe(mimizan_core::recipe::Recipe::on_paper());
        assert_eq!(mono.plate, None);
        assert_eq!(mono.lamps, [0.0; 2]);
        assert_eq!(mono.weights, e.weights, "no mix in the file keeps the weights");
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
