//! Input key model — the abstract keystrokes the frontend (Ratatui) feeds in.
//!
//! The frontend translates crossterm events into [`Key`]s. This stands in for
//! the raw byte / `K_SPECIAL` decoding that `getchar.c` does; the typeahead /
//! `:map` expansion layer sits between this and mode dispatch
//! ([`crate::keymap`]).
//!
//! ## How modifiers are encoded
//!
//! Shift on a character is folded into the character's *case*, which is what
//! terminals actually report: `<C-j>` is `Ctrl('j')` and `<C-S-j>` is
//! `Ctrl('J')`. That keeps the common `Key::Ctrl(c)` pattern working unchanged
//! while making the shifted form expressible. `<A-…>` and `<M-…>` are the same
//! key ([`Key::Alt`]), as in Vim.
//!
//! Keys that aren't characters at all — arrows, F-keys, Home/End — carry a
//! full [`Mods`] set instead, because there is no case to fold shift into.
//!
//! Ctrl and Alt together on a *character* (`<C-A-x>`) is deliberately not
//! representable: no terminal reports it portably, and admitting a second
//! encoding of an already-representable key would break mapping lookup, which
//! compares [`Key`]s for equality. [`Key::try_parse_sequence`] rejects it.

/// A key that isn't a character: modifiers can't be folded into its case, so
/// it carries them alongside (see [`Key::Special`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SpecialKey {
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Delete,
    Insert,
    /// Shift-Tab. Terminals report this as its own code rather than as Tab
    /// with a shift flag, so it is its own key here too.
    BackTab,
    /// `<F1>`–`<F12>`.
    F(u8),
}

impl SpecialKey {
    /// The `<...>` name for this key, without modifiers or angle brackets.
    fn name(self) -> String {
        match self {
            SpecialKey::Up => "Up".into(),
            SpecialKey::Down => "Down".into(),
            SpecialKey::Left => "Left".into(),
            SpecialKey::Right => "Right".into(),
            SpecialKey::Home => "Home".into(),
            SpecialKey::End => "End".into(),
            SpecialKey::PageUp => "PageUp".into(),
            SpecialKey::PageDown => "PageDown".into(),
            SpecialKey::Delete => "Del".into(),
            SpecialKey::Insert => "Insert".into(),
            SpecialKey::BackTab => "S-Tab".into(),
            SpecialKey::F(n) => format!("F{n}"),
        }
    }

    /// Parse a bare key name (already lowercased), e.g. `"up"`, `"pgdn"`, `"f5"`.
    fn parse(name: &str) -> Option<SpecialKey> {
        Some(match name {
            "up" => SpecialKey::Up,
            "down" => SpecialKey::Down,
            "left" => SpecialKey::Left,
            "right" => SpecialKey::Right,
            "home" => SpecialKey::Home,
            "end" => SpecialKey::End,
            "pageup" | "pgup" => SpecialKey::PageUp,
            "pagedown" | "pgdn" => SpecialKey::PageDown,
            "del" | "delete" => SpecialKey::Delete,
            "insert" | "ins" => SpecialKey::Insert,
            _ => {
                let n: u8 = name.strip_prefix('f')?.parse().ok()?;
                if (1..=12).contains(&n) {
                    SpecialKey::F(n)
                } else {
                    return None;
                }
            }
        })
    }
}

/// Modifier flags carried by a [`Key::Special`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Mods {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl Mods {
    /// No modifiers held.
    pub const NONE: Mods = Mods { ctrl: false, alt: false, shift: false };

    /// True when no modifier is held.
    pub fn is_none(self) -> bool {
        self == Mods::NONE
    }

    /// The `C-`/`A-`/`S-` prefix string for [`Key::notation`], in Vim's order.
    fn prefix(self) -> String {
        let mut s = String::new();
        if self.ctrl {
            s.push_str("C-");
        }
        if self.alt {
            s.push_str("A-");
        }
        if self.shift {
            s.push_str("S-");
        }
        s
    }
}

/// An abstract keystroke.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Key {
    /// A printable character.
    Char(char),
    /// A Ctrl-modified character. Case-significant: `Ctrl('r')` is `<C-r>`,
    /// `Ctrl('R')` is `<C-S-r>`. See the module docs on why shift folds into
    /// the case rather than into a flag.
    Ctrl(char),
    /// An Alt/Meta-modified character — `<A-j>` and `<M-j>` are the same key.
    /// Case-significant in the same way [`Key::Ctrl`] is.
    Alt(char),
    /// A non-character key, with its modifiers.
    Special { key: SpecialKey, mods: Mods },
    Esc,
    Enter,
    Backspace,
    Tab,
}

impl Key {
    /// Parse a single char as a printable key (convenience for tests/demos).
    pub fn ch(c: char) -> Key {
        Key::Char(c)
    }

    /// A [`Key::Special`] with no modifiers held.
    pub fn special(key: SpecialKey) -> Key {
        Key::Special { key, mods: Mods::NONE }
    }

    /// Render a key back into [`Key::parse_sequence`] notation. Recorded macros
    /// are stored in registers as text, exactly as Vim does — so a macro can be
    /// pasted out with `"ap`, edited by hand, and yanked back in. Every variant
    /// round-trips through [`Key::try_parse_sequence`].
    pub fn notation(self) -> String {
        match self {
            Key::Char(c) => c.to_string(),
            Key::Ctrl(c) => {
                if c.is_ascii_uppercase() {
                    format!("<C-S-{}>", c.to_ascii_lowercase())
                } else {
                    format!("<C-{c}>")
                }
            }
            Key::Alt(c) => {
                if c.is_ascii_uppercase() {
                    format!("<A-S-{}>", c.to_ascii_lowercase())
                } else {
                    format!("<A-{c}>")
                }
            }
            Key::Special { key, mods } => {
                // BackTab already spells its own shift, so it never takes a
                // prefix of its own.
                if key == SpecialKey::BackTab && mods.is_none() {
                    "<S-Tab>".to_string()
                } else {
                    format!("<{}{}>", mods.prefix(), key.name())
                }
            }
            Key::Esc => "<Esc>".to_string(),
            Key::Enter => "<CR>".to_string(),
            Key::Backspace => "<BS>".to_string(),
            Key::Tab => "<Tab>".to_string(),
        }
    }

    /// Translate a string into a key sequence, using `<...>` for specials:
    /// `<Esc>`, `<CR>`, `<BS>`, `<Tab>`, `<C-r>`, `<A-j>`, `<C-S-k>`, `<Up>`,
    /// `<C-Up>`, `<F5>`, `<S-Tab>`. Everything else is a literal char.
    ///
    /// This is the lossy form: an unrecognized `<...>` tag is taken literally,
    /// character by character. That is what register text needs, since a
    /// recorded macro may legitimately contain a typed `<`. Anywhere a *user*
    /// wrote the sequence — a mapping's lhs/rhs, config, `:map` — use
    /// [`Key::try_parse_sequence`] instead, so a typo is reported rather than
    /// silently becoming five keystrokes.
    pub fn parse_sequence(s: &str) -> Vec<Key> {
        Self::parse_seq(s, ' ', false).unwrap_or_default()
    }

    /// Like [`Key::parse_sequence`], but an unrecognized `<...>` tag is an
    /// error naming the offending tag.
    pub fn try_parse_sequence(s: &str) -> Result<Vec<Key>, String> {
        Self::parse_seq(s, ' ', true)
    }

    /// [`Key::parse_sequence`] with `<leader>` resolving to `leader`.
    pub fn parse_sequence_with_leader(s: &str, leader: char) -> Vec<Key> {
        Self::parse_seq(s, leader, false).unwrap_or_default()
    }

    /// Like [`Key::try_parse_sequence`], with `<leader>` resolving to `leader`
    /// rather than to Space (`mapleader`).
    pub fn try_parse_sequence_with_leader(s: &str, leader: char) -> Result<Vec<Key>, String> {
        Self::parse_seq(s, leader, true)
    }

    fn parse_seq(s: &str, leader: char, strict: bool) -> Result<Vec<Key>, String> {
        let mut out = Vec::new();
        let chars: Vec<char> = s.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            if chars[i] == '<' {
                // The tag ends at the first `>`; `<C-\>` and `<M-]>` mean the
                // base char can itself be punctuation, but never `>` — spell
                // that `<C-gt>`-free, as Vim does, by writing it literally.
                if let Some(close) = chars[i..].iter().position(|&c| c == '>') {
                    let tag: String = chars[i + 1..i + close].iter().collect();
                    match Self::parse_tag(&tag, leader) {
                        Ok(k) => {
                            out.push(k);
                            i += close + 1;
                            continue;
                        }
                        Err(e) if strict => return Err(e),
                        Err(_) => {} // lossy: fall through to a literal `<`
                    }
                }
            }
            out.push(Key::Char(chars[i]));
            i += 1;
        }
        Ok(out)
    }

    /// Parse the inside of a `<...>` tag: any number of `C-`/`A-`/`M-`/`S-`
    /// prefixes in any order, then a base key (one character or a name).
    fn parse_tag(tag: &str, leader: char) -> Result<Key, String> {
        let err = || format!("E1000: unrecognized key notation: <{tag}>");
        if tag.is_empty() {
            return Err(err());
        }

        let mut mods = Mods::NONE;
        let mut rest = tag;
        // Strip modifier prefixes. A prefix is only a prefix when something
        // follows it, so `<C->` and the bare char `<->` stay unambiguous.
        loop {
            let mut it = rest.chars();
            let (Some(c), Some('-')) = (it.next(), it.next()) else { break };
            let tail = &rest[c.len_utf8() + 1..];
            if tail.is_empty() {
                break;
            }
            match c.to_ascii_lowercase() {
                'c' => mods.ctrl = true,
                'a' | 'm' => mods.alt = true,
                's' => mods.shift = true,
                _ => break,
            }
            rest = tail;
        }

        let lower = rest.to_ascii_lowercase();

        // Named keys that stand alone (no modifiers folded in).
        let plain = match lower.as_str() {
            "esc" => Some(Key::Esc),
            "cr" | "enter" | "return" => Some(Key::Enter),
            "bs" => Some(Key::Backspace),
            "tab" => Some(Key::Tab),
            "space" => Some(Key::Char(' ')),
            "leader" => Some(Key::Char(leader)),
            "bar" => Some(Key::Char('|')),
            "lt" => Some(Key::Char('<')),
            "bslash" => Some(Key::Char('\\')),
            _ => None,
        };
        if let Some(k) = plain {
            // Shift-Tab is its own key rather than Tab carrying a flag, so
            // `<S-Tab>` has to normalize onto it — otherwise it would parse as
            // a plain `<Tab>` with the shift silently dropped.
            if k == Key::Tab && mods.shift {
                return Ok(Key::Special {
                    key: SpecialKey::BackTab,
                    mods: Mods { shift: false, ..mods },
                });
            }
            return Self::apply_mods(k, mods, tag);
        }

        // Non-character keys carry their modifiers directly.
        if let Some(key) = SpecialKey::parse(&lower) {
            return Ok(Key::Special { key, mods });
        }

        // A single base character.
        let mut it = rest.chars();
        let (Some(c), None) = (it.next(), it.next()) else {
            return Err(err());
        };
        Self::apply_mods(Key::Char(c), mods, tag)
    }

    /// Fold `mods` into a character-ish base key.
    fn apply_mods(base: Key, mods: Mods, tag: &str) -> Result<Key, String> {
        if mods.ctrl && mods.alt {
            return Err(format!(
                "E1001: Ctrl and Alt together is not representable: <{tag}>"
            ));
        }
        let c = match base {
            Key::Char(c) => c,
            // `<C-Tab>`, `<S-CR>` and friends: no terminal reports these
            // distinctly without the enhancement protocol, and nothing binds
            // them today, so the modifiers are dropped rather than inventing
            // an encoding for them.
            other => return Ok(other),
        };
        // Shift folds into case: `<S-a>` is `A`, `<C-S-j>` is `Ctrl('J')`.
        let c = if mods.shift { c.to_ascii_uppercase() } else { c };
        Ok(if mods.ctrl {
            Key::Ctrl(c)
        } else if mods.alt {
            Key::Alt(c)
        } else {
            Key::Char(c)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_specials() {
        let keys = Key::parse_sequence("dw<Esc>");
        assert_eq!(keys, vec![Key::Char('d'), Key::Char('w'), Key::Esc]);
    }

    #[test]
    fn parse_ctrl() {
        let keys = Key::parse_sequence("<C-r>u");
        assert_eq!(keys, vec![Key::Ctrl('r'), Key::Char('u')]);
    }

    #[test]
    fn parse_space_and_leader() {
        assert_eq!(Key::parse_sequence("<Space>w"), vec![Key::Char(' '), Key::Char('w')]);
        assert_eq!(Key::parse_sequence("<leader>e"), vec![Key::Char(' '), Key::Char('e')]);
    }

    #[test]
    fn leader_is_configurable() {
        assert_eq!(
            Key::try_parse_sequence_with_leader("<leader>e", ',').unwrap(),
            vec![Key::Char(','), Key::Char('e')]
        );
    }

    #[test]
    fn parse_alt_and_meta_are_the_same_key() {
        assert_eq!(Key::parse_sequence("<A-j>"), vec![Key::Alt('j')]);
        assert_eq!(Key::parse_sequence("<M-j>"), vec![Key::Alt('j')]);
        // The bindings charvim actually uses.
        assert_eq!(Key::parse_sequence("<M-1>"), vec![Key::Alt('1')]);
        assert_eq!(Key::parse_sequence("<M-[>"), vec![Key::Alt('[')]);
        assert_eq!(Key::parse_sequence("<M-]>"), vec![Key::Alt(']')]);
    }

    #[test]
    fn shift_folds_into_case() {
        assert_eq!(Key::parse_sequence("<C-S-j>"), vec![Key::Ctrl('J')]);
        assert_eq!(Key::parse_sequence("<S-C-j>"), vec![Key::Ctrl('J')], "order-independent");
        assert_eq!(Key::parse_sequence("<A-S-k>"), vec![Key::Alt('K')]);
        assert_eq!(Key::parse_sequence("<S-a>"), vec![Key::Char('A')]);
        // ...and the unshifted forms stay distinct, which is the whole point.
        assert_ne!(Key::parse_sequence("<C-j>"), Key::parse_sequence("<C-S-j>"));
    }

    #[test]
    fn parse_non_character_keys() {
        assert_eq!(Key::parse_sequence("<Up>"), vec![Key::special(SpecialKey::Up)]);
        assert_eq!(Key::parse_sequence("<F5>"), vec![Key::special(SpecialKey::F(5))]);
        assert_eq!(Key::parse_sequence("<Del>"), vec![Key::special(SpecialKey::Delete)]);
        assert_eq!(Key::parse_sequence("<S-Tab>"), vec![Key::special(SpecialKey::BackTab)]);
        assert_eq!(
            Key::parse_sequence("<C-Up>"),
            vec![Key::Special { key: SpecialKey::Up, mods: Mods { ctrl: true, ..Mods::NONE } }]
        );
    }

    #[test]
    fn punctuation_base_chars() {
        assert_eq!(Key::parse_sequence("<C-\\>"), vec![Key::Ctrl('\\')]);
        assert_eq!(Key::parse_sequence("<Bar>"), vec![Key::Char('|')]);
        assert_eq!(Key::parse_sequence("<lt>"), vec![Key::Char('<')]);
    }

    #[test]
    fn every_key_round_trips_through_notation() {
        let keys = [
            Key::Char('x'),
            Key::Char(' '),
            Key::Ctrl('r'),
            Key::Ctrl('J'),
            Key::Alt('j'),
            Key::Alt('K'),
            Key::Alt('['),
            Key::special(SpecialKey::Up),
            Key::special(SpecialKey::BackTab),
            Key::special(SpecialKey::F(12)),
            Key::Special { key: SpecialKey::Down, mods: Mods { ctrl: true, ..Mods::NONE } },
            Key::Special { key: SpecialKey::End, mods: Mods { alt: true, shift: true, ctrl: false } },
            Key::Esc,
            Key::Enter,
            Key::Backspace,
            Key::Tab,
        ];
        for k in keys {
            let text = k.notation();
            assert_eq!(
                Key::try_parse_sequence(&text).unwrap(),
                vec![k],
                "{k:?} did not round-trip through {text:?}"
            );
        }
    }

    #[test]
    fn strict_parse_rejects_an_unknown_tag() {
        // The silent-degradation bug: `<A-x>` used to become five literal
        // chars, so a mistyped mapping failed invisibly.
        let e = Key::try_parse_sequence("<Nope>").unwrap_err();
        assert!(e.contains("<Nope>"), "{e}");
        assert!(Key::try_parse_sequence("<C-A-x>").is_err(), "ctrl+alt is not representable");
    }

    #[test]
    fn lossy_parse_keeps_literal_angle_brackets() {
        // Register text can contain a genuinely typed `<`.
        assert_eq!(
            Key::parse_sequence("a<b"),
            vec![Key::Char('a'), Key::Char('<'), Key::Char('b')]
        );
        assert_eq!(Key::parse_sequence("<Nope>").len(), 6);
    }
}
