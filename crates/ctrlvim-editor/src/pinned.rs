//! A short, ordered list of files pinned for instant access — the idea behind
//! harpoon, which most Vim configs (including the one this was modelled on)
//! lean on heavily.
//!
//! It exists because neither of Vim's two nearby features covers the need.
//! Marks are *positions* and are per-file, so they can't answer "the four files
//! I'm working in right now". The buffer list does hold files, but its order is
//! whatever you happened to open, and it grows without bound — so `:b 7` means
//! something different by lunchtime. A pin list is deliberately small, ordered
//! by the user, and stable: slot 2 stays slot 2 until it's moved.
//!
//! The engine owns the list; opening a file and drawing the menu are the
//! host's, the same split quickfix and tags use.

use std::path::{Path, PathBuf};

/// An ordered set of pinned files, plus which one is current.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PinList {
    files: Vec<PathBuf>,
    /// Index into `files` of the pin most recently jumped to, so `:PinNext`
    /// and `:PinPrev` have somewhere to start from.
    current: usize,
}

impl PinList {
    pub fn new() -> PinList {
        PinList::default()
    }

    /// The pinned files, in order.
    pub fn files(&self) -> &[PathBuf] {
        &self.files
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    /// Index of the pin last jumped to.
    pub fn current(&self) -> usize {
        self.current
    }

    /// Pin `path`, returning its 1-based slot.
    ///
    /// Pinning something already pinned is a no-op that returns the existing
    /// slot rather than adding a duplicate — the slot number is the whole
    /// value of the list, so it must not shift under the user for a keystroke
    /// that felt idempotent.
    pub fn add(&mut self, path: impl AsRef<Path>) -> usize {
        let path = path.as_ref();
        if let Some(i) = self.index_of(path) {
            self.current = i;
            return i + 1;
        }
        self.files.push(path.to_path_buf());
        self.current = self.files.len() - 1;
        self.files.len()
    }

    /// Unpin `path`. Returns whether it was pinned.
    pub fn remove(&mut self, path: impl AsRef<Path>) -> bool {
        let Some(i) = self.index_of(path.as_ref()) else { return false };
        self.files.remove(i);
        // Keep `current` pointing inside the list, so a later `:PinNext` after
        // removing the last entry doesn't start from out of bounds.
        self.current = self.current.min(self.files.len().saturating_sub(1));
        true
    }

    /// Unpin everything.
    pub fn clear(&mut self) {
        self.files.clear();
        self.current = 0;
    }

    /// The file in 1-based slot `n`, if there is one. Also marks it current, so
    /// `:PinNext` continues from where the jump landed.
    pub fn go(&mut self, n: usize) -> Option<PathBuf> {
        let i = n.checked_sub(1)?;
        let f = self.files.get(i)?.clone();
        self.current = i;
        Some(f)
    }

    /// The next pin, wrapping. `None` when nothing is pinned.
    pub fn next(&mut self) -> Option<PathBuf> {
        self.step(1)
    }

    /// The previous pin, wrapping.
    pub fn prev(&mut self) -> Option<PathBuf> {
        self.step(-1)
    }

    fn step(&mut self, by: isize) -> Option<PathBuf> {
        if self.files.is_empty() {
            return None;
        }
        let len = self.files.len() as isize;
        self.current = (((self.current as isize + by) % len + len) % len) as usize;
        self.files.get(self.current).cloned()
    }

    /// 1-based slot of `path`, if pinned — for showing "2/4" in a status line.
    pub fn slot_of(&self, path: impl AsRef<Path>) -> Option<usize> {
        self.index_of(path.as_ref()).map(|i| i + 1)
    }

    fn index_of(&self, path: &Path) -> Option<usize> {
        self.files.iter().position(|p| p == path)
    }

    /// Replace the whole list, e.g. when restoring a saved session. Anything
    /// out of range is dropped rather than trusted.
    pub fn set(&mut self, files: Vec<PathBuf>) {
        self.files = files;
        self.current = self.current.min(self.files.len().saturating_sub(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(paths: &[&str]) -> PinList {
        let mut l = PinList::new();
        for p in paths {
            l.add(p);
        }
        l
    }

    #[test]
    fn add_assigns_stable_slots() {
        let mut l = PinList::new();
        assert_eq!(l.add("a.rs"), 1);
        assert_eq!(l.add("b.rs"), 2);
        assert_eq!(l.add("c.rs"), 3);
        assert_eq!(l.go(2).unwrap(), PathBuf::from("b.rs"));
    }

    #[test]
    fn pinning_the_same_file_twice_does_not_shift_the_slots() {
        // The slot number is the point of the list; a second `<leader>a` on a
        // file you already pinned must not renumber everything after it.
        let mut l = list(&["a.rs", "b.rs"]);
        assert_eq!(l.add("a.rs"), 1, "the slot it already had");
        assert_eq!(l.len(), 2);
        assert_eq!(l.files(), [PathBuf::from("a.rs"), PathBuf::from("b.rs")]);
    }

    #[test]
    fn remove_reports_whether_it_was_pinned() {
        let mut l = list(&["a.rs", "b.rs"]);
        assert!(l.remove("a.rs"));
        assert!(!l.remove("a.rs"), "already gone");
        assert_eq!(l.files(), [PathBuf::from("b.rs")]);
    }

    #[test]
    fn next_and_prev_wrap() {
        let mut l = list(&["a.rs", "b.rs", "c.rs"]);
        l.go(1);
        assert_eq!(l.next().unwrap(), PathBuf::from("b.rs"));
        assert_eq!(l.next().unwrap(), PathBuf::from("c.rs"));
        assert_eq!(l.next().unwrap(), PathBuf::from("a.rs"), "wraps forward");
        assert_eq!(l.prev().unwrap(), PathBuf::from("c.rs"), "and backward");
    }

    #[test]
    fn navigating_an_empty_list_is_not_an_error() {
        let mut l = PinList::new();
        assert!(l.next().is_none());
        assert!(l.prev().is_none());
        assert!(l.go(1).is_none());
    }

    #[test]
    fn go_is_one_based_and_bounded() {
        let mut l = list(&["a.rs", "b.rs"]);
        assert!(l.go(0).is_none(), "there is no slot 0");
        assert!(l.go(3).is_none());
        assert_eq!(l.go(1).unwrap(), PathBuf::from("a.rs"));
    }

    #[test]
    fn removing_the_current_pin_leaves_the_cursor_in_range() {
        // `current` used to be able to dangle past the end, so the next
        // `:PinNext` started from out of bounds.
        let mut l = list(&["a.rs", "b.rs"]);
        l.go(2);
        assert!(l.remove("b.rs"));
        assert_eq!(l.current(), 0);
        assert_eq!(l.next().unwrap(), PathBuf::from("a.rs"));
    }

    #[test]
    fn slot_of_finds_the_pin() {
        let l = list(&["a.rs", "b.rs"]);
        assert_eq!(l.slot_of("b.rs"), Some(2));
        assert_eq!(l.slot_of("nope.rs"), None);
    }

    #[test]
    fn clear_resets_everything() {
        let mut l = list(&["a.rs", "b.rs"]);
        l.go(2);
        l.clear();
        assert!(l.is_empty());
        assert_eq!(l.current(), 0);
    }
}
