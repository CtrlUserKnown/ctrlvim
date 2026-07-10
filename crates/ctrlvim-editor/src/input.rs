//! Input key model — the abstract keystrokes the frontend (Ratatui) feeds in.
//!
//! This is intentionally minimal: the Ratatui frontend translates crossterm
//! events into [`Key`]s. It stands in for the raw byte / `K_SPECIAL` decoding
//! that `getchar.c` does; the typeahead/`:map` expansion layer (M3) will sit
//! between this and mode dispatch.

/// An abstract keystroke.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// A printable character.
    Char(char),
    /// A Ctrl-modified letter, e.g. `Ctrl('r')` for `<C-r>`.
    Ctrl(char),
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

    /// Translate a string into a key sequence, using `<...>` for specials:
    /// `<Esc>`, `<CR>`, `<BS>`, `<Tab>`, `<C-r>`. Everything else is a literal
    /// char. This lets tests and scripts write `"dw<Esc>"` style input.
    pub fn parse_sequence(s: &str) -> Vec<Key> {
        let mut out = Vec::new();
        let bytes: Vec<char> = s.chars().collect();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == '<' {
                if let Some(close) = bytes[i..].iter().position(|&c| c == '>') {
                    let tag: String = bytes[i + 1..i + close].iter().collect();
                    let key = match tag.to_ascii_lowercase().as_str() {
                        "esc" => Some(Key::Esc),
                        "cr" | "enter" => Some(Key::Enter),
                        "bs" => Some(Key::Backspace),
                        "tab" => Some(Key::Tab),
                        _ if tag.len() == 3 && tag.to_ascii_lowercase().starts_with("c-") => {
                            Some(Key::Ctrl(tag.chars().nth(2).unwrap()))
                        }
                        _ => None,
                    };
                    if let Some(k) = key {
                        out.push(k);
                        i += close + 1;
                        continue;
                    }
                }
            }
            out.push(Key::Char(bytes[i]));
            i += 1;
        }
        out
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
}
