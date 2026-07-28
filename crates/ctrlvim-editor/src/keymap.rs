//! A native key-mapping table with the beginnings of typeahead expansion — the
//! foundation of Neovim's `:map`/`getchar.c` layer (roadmap M3).
//!
//! A mapping is a left-hand key sequence that expands to a right-hand key
//! sequence (`:nnoremap lhs rhs`). Because the right-hand side is re-fed through
//! the session, a `<leader>` chord like `<Space>w` can expand to a real command
//! line such as `:w<CR>` — the same way a Neovim user's config would define it,
//! rather than the frontend hard-coding the chord. Mappings are non-recursive
//! (noremap) for now; the full timeout/ambiguity machinery is still M3.

use crate::input::Key;

/// One normal-mode mapping: `lhs` keys expand to `rhs` keys.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mapping {
    pub lhs: Vec<Key>,
    pub rhs: Vec<Key>,
}

/// The result of matching buffered keys against the mapping table.
pub enum KeymapMatch {
    /// The buffer exactly matches a mapping; expand to these keys.
    Full(Vec<Key>),
    /// The buffer is a strict prefix of at least one mapping; wait for more.
    Prefix,
    /// No mapping matches; the keys should be used literally.
    None,
}

/// Which mode a mapping applies in — the `n`/`i`/`v` of `:nmap`/`:imap`/`:vmap`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MapMode {
    Normal,
    Insert,
    Visual,
}

impl MapMode {
    /// Parse the mode letter used by `:map` commands and the config's
    /// `[[keymap]] mode` field. Unknown letters fall back to normal, matching
    /// how a bare `:map` behaves.
    pub fn parse(s: &str) -> MapMode {
        match s.trim() {
            "i" | "insert" => MapMode::Insert,
            "v" | "x" | "visual" => MapMode::Visual,
            _ => MapMode::Normal,
        }
    }
}

/// Per-mode mapping tables.
#[derive(Default)]
pub struct Keymap {
    normal: Vec<Mapping>,
    insert: Vec<Mapping>,
    visual: Vec<Mapping>,
}

impl Keymap {
    fn table(&self, mode: MapMode) -> &Vec<Mapping> {
        match mode {
            MapMode::Normal => &self.normal,
            MapMode::Insert => &self.insert,
            MapMode::Visual => &self.visual,
        }
    }

    fn table_mut(&mut self, mode: MapMode) -> &mut Vec<Mapping> {
        match mode {
            MapMode::Normal => &mut self.normal,
            MapMode::Insert => &mut self.insert,
            MapMode::Visual => &mut self.visual,
        }
    }

    /// Set a mapping in `mode`, `lhs`/`rhs` written in `<...>` key notation
    /// (`<Space>w` → `:w<CR>`). Replaces any existing mapping with the same lhs.
    pub fn set(&mut self, mode: MapMode, lhs: &str, rhs: &str) {
        let lhs = Key::parse_sequence(lhs);
        let rhs = Key::parse_sequence(rhs);
        let table = self.table_mut(mode);
        table.retain(|m| m.lhs != lhs);
        table.push(Mapping { lhs, rhs });
    }

    /// Set a normal-mode mapping.
    pub fn set_normal(&mut self, lhs: &str, rhs: &str) {
        self.set(MapMode::Normal, lhs, rhs);
    }

    /// Remove a mapping (`:unmap`). Returns whether one was there.
    pub fn remove(&mut self, mode: MapMode, lhs: &str) -> bool {
        let lhs = Key::parse_sequence(lhs);
        let table = self.table_mut(mode);
        let before = table.len();
        table.retain(|m| m.lhs != lhs);
        table.len() != before
    }

    /// Match `pending` against `mode`'s mappings.
    pub fn match_mode(&self, mode: MapMode, pending: &[Key]) -> KeymapMatch {
        let mut has_prefix = false;
        for m in self.table(mode) {
            if m.lhs == pending {
                return KeymapMatch::Full(m.rhs.clone());
            }
            if m.lhs.len() > pending.len() && m.lhs.starts_with(pending) {
                has_prefix = true;
            }
        }
        if has_prefix {
            KeymapMatch::Prefix
        } else {
            KeymapMatch::None
        }
    }

    /// Match `pending` against the normal-mode mappings.
    pub fn match_normal(&self, pending: &[Key]) -> KeymapMatch {
        self.match_mode(MapMode::Normal, pending)
    }

    /// True when `key` on its own could begin a mapping in `mode`.
    pub fn can_start(&self, mode: MapMode, key: Key) -> bool {
        !matches!(self.match_mode(mode, &[key]), KeymapMatch::None)
    }

    /// True when `key` on its own could begin a normal-mode mapping.
    pub fn can_start_normal(&self, key: Key) -> bool {
        self.can_start(MapMode::Normal, key)
    }

    /// Every mapping in `mode`, for `:map` with no arguments.
    pub fn list(&self, mode: MapMode) -> &[Mapping] {
        self.table(mode)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Vec<Key> {
        Key::parse_sequence(s)
    }

    #[test]
    fn full_prefix_none() {
        let mut km = Keymap::default();
        km.set_normal("<Space>w", ":w<CR>");
        km.set_normal("<Space>ff", ":Files<CR>");

        assert!(matches!(km.match_normal(&parse("<Space>")), KeymapMatch::Prefix));
        assert!(matches!(km.match_normal(&parse("<Space>f")), KeymapMatch::Prefix));
        match km.match_normal(&parse("<Space>w")) {
            KeymapMatch::Full(rhs) => assert_eq!(rhs, parse(":w<CR>")),
            _ => panic!("expected full match"),
        }
        assert!(matches!(km.match_normal(&parse("<Space>x")), KeymapMatch::None));
        assert!(matches!(km.match_normal(&parse("x")), KeymapMatch::None));
    }

    #[test]
    fn modes_have_independent_tables() {
        let mut km = Keymap::default();
        km.set(MapMode::Insert, "jk", "<Esc>");
        // The insert mapping must not leak into normal mode.
        assert!(matches!(km.match_normal(&parse("jk")), KeymapMatch::None));
        match km.match_mode(MapMode::Insert, &parse("jk")) {
            KeymapMatch::Full(rhs) => assert_eq!(rhs, parse("<Esc>")),
            _ => panic!("expected full match"),
        }
    }

    #[test]
    fn mode_letters_parse() {
        assert_eq!(MapMode::parse("i"), MapMode::Insert);
        assert_eq!(MapMode::parse("v"), MapMode::Visual);
        assert_eq!(MapMode::parse("x"), MapMode::Visual);
        assert_eq!(MapMode::parse("n"), MapMode::Normal);
        assert_eq!(MapMode::parse("garbage"), MapMode::Normal);
    }

    #[test]
    fn unmap_removes_a_mapping() {
        let mut km = Keymap::default();
        km.set_normal("<Space>w", ":w<CR>");
        assert!(km.remove(MapMode::Normal, "<Space>w"));
        assert!(!km.remove(MapMode::Normal, "<Space>w"), "already gone");
        assert!(matches!(km.match_normal(&parse("<Space>w")), KeymapMatch::None));
    }

    #[test]
    fn set_normal_replaces() {
        let mut km = Keymap::default();
        km.set_normal("<Space>w", ":w<CR>");
        km.set_normal("<Space>w", ":wq<CR>");
        match km.match_normal(&parse("<Space>w")) {
            KeymapMatch::Full(rhs) => assert_eq!(rhs, parse(":wq<CR>")),
            _ => panic!("expected full match"),
        }
    }
}
