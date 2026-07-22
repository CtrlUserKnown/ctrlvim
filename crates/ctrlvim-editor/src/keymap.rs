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

/// Normal-mode mappings. (Other modes join this table as they gain maps.)
#[derive(Default)]
pub struct Keymap {
    normal: Vec<Mapping>,
}

impl Keymap {
    /// Set a normal-mode mapping, `lhs`/`rhs` written in `<...>` key notation
    /// (`<Space>w` → `:w<CR>`). Replaces any existing mapping with the same lhs.
    pub fn set_normal(&mut self, lhs: &str, rhs: &str) {
        let lhs = Key::parse_sequence(lhs);
        let rhs = Key::parse_sequence(rhs);
        self.normal.retain(|m| m.lhs != lhs);
        self.normal.push(Mapping { lhs, rhs });
    }

    /// Match `pending` against the normal-mode mappings.
    pub fn match_normal(&self, pending: &[Key]) -> KeymapMatch {
        let mut has_prefix = false;
        for m in &self.normal {
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

    /// True when `key` on its own could begin a normal-mode mapping.
    pub fn can_start_normal(&self, key: Key) -> bool {
        !matches!(self.match_normal(&[key]), KeymapMatch::None)
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
