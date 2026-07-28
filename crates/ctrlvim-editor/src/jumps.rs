//! The jumplist — `<C-o>`/`<C-i>` and `:jumps`, Neovim's `mark.c` jumplist.
//!
//! Vim keeps one jumplist per window, recording the cursor position *before*
//! any "far" motion (`G`, `gg`, a search, a mark jump, `:{line}`). `<C-o>` walks
//! back through it and `<C-i>` walks forward again, so the list behaves like a
//! browser history rather than a stack.

use ctrlvim_types::{BufferId, Position};

/// How many entries Vim keeps (`'jumpoptions'` aside, this is the fixed limit).
const MAX_JUMPS: usize = 100;

/// One recorded position. The buffer is part of the entry because a jump can
/// cross files (`Ctrl-]`, `:e`), and coming back must reopen the right one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Jump {
    pub buffer: BufferId,
    pub pos: Position,
}

impl Jump {
    pub fn new(buffer: BufferId, pos: Position) -> Self {
        Jump { buffer, pos }
    }
}

/// A window's jumplist.
#[derive(Debug, Default, Clone)]
pub struct Jumplist {
    entries: Vec<Jump>,
    /// Index of the current position within `entries`. Equal to `entries.len()`
    /// when sitting at the newest position (i.e. not currently traversing).
    idx: usize,
}

impl Jumplist {
    /// Record a position being jumped *away from*. Any forward history is
    /// discarded, exactly as a new navigation does in a browser.
    pub fn push(&mut self, jump: Jump) {
        self.entries.truncate(self.idx);
        // Vim collapses a repeat of the same spot rather than stacking it.
        if self.entries.last() == Some(&jump) {
            self.idx = self.entries.len();
            return;
        }
        self.entries.push(jump);
        if self.entries.len() > MAX_JUMPS {
            self.entries.remove(0);
        }
        self.idx = self.entries.len();
    }

    /// `<C-o>` — step to an older entry. `current` is where the cursor is now;
    /// on the first step it gets appended so `<C-i>` has somewhere to return to.
    pub fn back(&mut self, current: Jump) -> Option<Jump> {
        if self.idx == 0 {
            return None;
        }
        if self.idx == self.entries.len() {
            self.entries.push(current);
        }
        self.idx -= 1;
        self.entries.get(self.idx).copied()
    }

    /// `<C-i>` — step to a newer entry.
    pub fn forward(&mut self) -> Option<Jump> {
        if self.idx + 1 >= self.entries.len() {
            return None;
        }
        self.idx += 1;
        self.entries.get(self.idx).copied()
    }

    /// Every entry, oldest first (`:jumps`).
    pub fn entries(&self) -> &[Jump] {
        &self.entries
    }

    /// Index of the current position, for `:jumps` to mark with `>`.
    pub fn current_index(&self) -> usize {
        self.idx
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(line: usize) -> Jump {
        Jump::new(BufferId(0), Position::new(line, 0))
    }

    #[test]
    fn back_and_forward_walk_the_list() {
        let mut j = Jumplist::default();
        j.push(at(1));
        j.push(at(2));
        // Sitting at line 3 having jumped from 2, then 1.
        assert_eq!(j.back(at(3)), Some(at(2)));
        assert_eq!(j.back(at(2)), Some(at(1)));
        assert_eq!(j.back(at(1)), None, "no entries older than the first");
        assert_eq!(j.forward(), Some(at(2)));
        assert_eq!(j.forward(), Some(at(3)), "returns to where <C-o> started");
        assert_eq!(j.forward(), None);
    }

    #[test]
    fn a_new_jump_discards_forward_history() {
        let mut j = Jumplist::default();
        j.push(at(1));
        j.push(at(2));
        j.back(at(3));
        // Now mid-list; a fresh jump truncates everything newer.
        j.push(at(9));
        assert_eq!(j.forward(), None);
        assert_eq!(j.back(at(10)), Some(at(9)));
    }

    #[test]
    fn repeated_pushes_of_one_spot_collapse() {
        let mut j = Jumplist::default();
        j.push(at(5));
        j.push(at(5));
        assert_eq!(j.entries().len(), 1);
    }

    #[test]
    fn the_list_is_capped() {
        let mut j = Jumplist::default();
        for line in 0..MAX_JUMPS + 20 {
            j.push(at(line));
        }
        assert_eq!(j.entries().len(), MAX_JUMPS);
        // The oldest entries fell off the front.
        assert_eq!(j.entries()[0], at(20));
    }
}
