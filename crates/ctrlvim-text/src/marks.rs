//! Unified mark store — the replacement for both `mark.c` (legacy line marks)
//! and `marktree.c`/`extmark.c` (extmarks).
//!
//! Per the plan, we unify *all* position tracking into one namespaced structure
//! from day one rather than replicating Neovim's two parallel systems. Legacy
//! named marks (`a`-`z`, `'<`, `'>`), the jumplist, and plugin/LSP extmarks all
//! live here as entries in namespaces.
//!
//! The C `marktree.c` is a custom in-memory B-tree for O(log n) splice-adjust
//! across thousands of marks. This implementation keeps the same *semantics*
//! (gravity-aware auto-adjustment on edits) with a straightforward flat store;
//! it can be swapped for a B-tree later without changing callers.

use ctrlvim_types::Position;
use std::collections::HashMap;

/// A namespace groups marks by owner (a plugin, LSP diagnostics, the legacy
/// mark set, etc.). Neovim allocates these via `ctrlvim_create_namespace`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Namespace(pub u32);

/// The namespace holding classic named marks (`ma`, `` `a ``, `'a`). Ids in it
/// are the mark character's code point, so `'a'` is id 97. Reserved: plugin
/// namespaces are allocated from 1 upward by `ctrlvim_create_namespace`.
pub const NS_LEGACY_MARKS: Namespace = Namespace(0);

/// Which way a mark moves when text is inserted exactly at its position.
/// Neovim's extmarks call this "gravity": right-gravity marks stay after
/// inserted text, left-gravity marks stay before it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gravity {
    Left,
    Right,
}

#[derive(Debug, Clone, Copy)]
struct Mark {
    pos: Position,
    gravity: Gravity,
}

/// The per-buffer mark store.
#[derive(Default)]
pub struct MarkStore {
    /// (namespace, id) -> mark.
    marks: HashMap<(u32, u32), Mark>,
    next_id: HashMap<u32, u32>,
}

impl MarkStore {
    pub fn new() -> Self {
        MarkStore::default()
    }

    /// Set (or overwrite) a mark with an explicit id (`ctrlvim_buf_set_extmark`
    /// with an `id`, or a legacy named mark keyed by its char code).
    pub fn set(&mut self, ns: Namespace, id: u32, pos: Position, gravity: Gravity) {
        self.marks.insert((ns.0, id), Mark { pos, gravity });
    }

    /// Set a mark with an auto-allocated id, returning it (`ctrlvim_buf_set_extmark`
    /// with `id = 0`).
    pub fn add(&mut self, ns: Namespace, pos: Position, gravity: Gravity) -> u32 {
        let counter = self.next_id.entry(ns.0).or_insert(1);
        let id = *counter;
        *counter += 1;
        self.marks.insert((ns.0, id), Mark { pos, gravity });
        id
    }

    /// Look up a mark's current position.
    pub fn get(&self, ns: Namespace, id: u32) -> Option<Position> {
        self.marks.get(&(ns.0, id)).map(|m| m.pos)
    }

    /// Remove a mark.
    pub fn remove(&mut self, ns: Namespace, id: u32) -> bool {
        self.marks.remove(&(ns.0, id)).is_some()
    }

    /// All marks in a namespace as `(id, pos)`, sorted by position — the
    /// ordering `ctrlvim_buf_get_extmarks` / `:marks` introspection relies on.
    pub fn all_in(&self, ns: Namespace) -> Vec<(u32, Position)> {
        let mut out: Vec<(u32, Position)> = self
            .marks
            .iter()
            .filter(|((n, _), _)| *n == ns.0)
            .map(|((_, id), m)| (*id, m.pos))
            .collect();
        out.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0)));
        out
    }

    /// Adjust all marks after a line-range edit: lines `[start, old_end)` were
    /// replaced with `new_line_count` lines. This is the equivalent of
    /// `mark_adjust`/`extmark_adjust` — but note that unlike the C legacy path,
    /// which linearly patches every scattered mark struct, here it is one pass
    /// over one store.
    pub fn adjust_lines(&mut self, start: usize, old_end: usize, new_line_count: usize) {
        let removed = old_end.saturating_sub(start);
        let delta = new_line_count as isize - removed as isize;
        for mark in self.marks.values_mut() {
            let line = mark.pos.line;
            if line < start {
                // before the edit: unaffected
            } else if line < old_end {
                // inside the deleted range: clamp to the edit start
                mark.pos.line = start;
                mark.pos.col = 0;
            } else {
                // after the edit: shift by the net line delta
                mark.pos.line = (line as isize + delta).max(0) as usize;
            }
        }
    }

    /// Adjust marks on the same line after an intra-line insert/delete at
    /// `col`, shifting `byte_delta` bytes (negative for deletes). Honors gravity
    /// for marks sitting exactly at the insertion column.
    pub fn adjust_cols(&mut self, line: usize, col: usize, byte_delta: isize) {
        for mark in self.marks.values_mut() {
            if mark.pos.line != line {
                continue;
            }
            if mark.pos.col > col || (mark.pos.col == col && mark.gravity == Gravity::Right) {
                mark.pos.col = (mark.pos.col as isize + byte_delta).max(0) as usize;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NS: Namespace = Namespace(1);

    #[test]
    fn set_get_remove() {
        let mut s = MarkStore::new();
        s.set(NS, 10, Position::new(2, 3), Gravity::Right);
        assert_eq!(s.get(NS, 10), Some(Position::new(2, 3)));
        assert!(s.remove(NS, 10));
        assert_eq!(s.get(NS, 10), None);
    }

    #[test]
    fn line_insert_shifts_later_marks() {
        let mut s = MarkStore::new();
        s.set(NS, 1, Position::new(5, 0), Gravity::Right);
        // Insert 2 lines at line 2 (replace [2,2) with 2 lines).
        s.adjust_lines(2, 2, 2);
        assert_eq!(s.get(NS, 1), Some(Position::new(7, 0)));
    }

    #[test]
    fn line_delete_clamps_marks_inside() {
        let mut s = MarkStore::new();
        s.set(NS, 1, Position::new(3, 4), Gravity::Right);
        // Delete lines [2,5) -> replace with 0 lines.
        s.adjust_lines(2, 5, 0);
        assert_eq!(s.get(NS, 1), Some(Position::new(2, 0)));
    }

    #[test]
    fn col_insert_respects_gravity() {
        let mut s = MarkStore::new();
        s.set(NS, 1, Position::new(0, 5), Gravity::Right);
        s.set(NS, 2, Position::new(0, 5), Gravity::Left);
        s.adjust_cols(0, 5, 3); // insert 3 bytes at col 5
        assert_eq!(s.get(NS, 1), Some(Position::new(0, 8))); // right moves
        assert_eq!(s.get(NS, 2), Some(Position::new(0, 5))); // left stays
    }

    #[test]
    fn all_in_is_position_sorted() {
        let mut s = MarkStore::new();
        s.set(NS, 1, Position::new(3, 0), Gravity::Right);
        s.set(NS, 2, Position::new(1, 0), Gravity::Right);
        let all = s.all_in(NS);
        assert_eq!(all[0].1.line, 1);
        assert_eq!(all[1].1.line, 3);
    }
}
