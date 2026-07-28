//! Search offsets — the `/pat/e+1` suffix on a search command.
//!
//! An offset does not change *what* matches, only where the cursor lands
//! afterwards, which is why it lives beside the engine rather than inside it.
//! It is parsed here so that `/`, `?`, `n` and `N` all agree on the meaning,
//! and so a repeat carries the offset along the way Vim's does.

/// Where to leave the cursor after a match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Offset {
    /// No offset: the cursor sits on the first character of the match.
    #[default]
    None,
    /// `/pat/+2`, `/pat/-1`, `/pat/3` — move whole lines, then to the first
    /// non-blank.
    Line(isize),
    /// `/pat/s+1`, `/pat/b-2` — relative to the match's first character.
    Start(isize),
    /// `/pat/e`, `/pat/e+1` — relative to the match's *last* character, which
    /// is why a bare `e` still moves the cursor.
    End(isize),
}

impl Offset {
    /// Parse the text after the closing delimiter of a search command.
    ///
    /// Anything unrecognized is [`Offset::None`] rather than an error: Vim
    /// tolerates a stray suffix, and refusing to search because of one would be
    /// worse than ignoring it.
    pub fn parse(s: &str) -> Offset {
        let s = s.trim();
        if s.is_empty() {
            return Offset::None;
        }
        let mut chars = s.chars();
        let first = chars.next().expect("non-empty");
        match first {
            'e' => Offset::End(parse_delta(chars.as_str())),
            's' | 'b' => Offset::Start(parse_delta(chars.as_str())),
            '+' | '-' | '0'..='9' => Offset::Line(parse_delta(s)),
            _ => Offset::None,
        }
    }

    /// Whether the offset moves by lines, which makes the search *linewise* —
    /// Vim treats `/pat/+1` as a linewise motion for operator purposes.
    pub fn is_line(self) -> bool {
        matches!(self, Offset::Line(_))
    }

    /// Apply a character offset to a match on one line.
    ///
    /// `start` and `end` are character columns, `end` exclusive. Returns the
    /// column to put the cursor on, or `None` for a line offset, which the
    /// caller has to resolve against the buffer.
    pub fn apply(self, start: usize, end: usize) -> Option<usize> {
        match self {
            Offset::None => Some(start),
            Offset::Line(_) => None,
            Offset::Start(n) => Some(shift(start, n)),
            // A bare `e` lands on the last character of the match, so the base
            // is one before the exclusive end.
            Offset::End(n) => Some(shift(end.saturating_sub(1), n)),
        }
    }
}

/// `+3`, `-2`, `3`, or an empty string meaning zero.
fn parse_delta(s: &str) -> isize {
    let s = s.trim();
    match s {
        "" => 0,
        "+" => 1,
        "-" => -1,
        _ => s.parse().unwrap_or(0),
    }
}

fn shift(base: usize, n: isize) -> usize {
    if n >= 0 {
        base.saturating_add(n as usize)
    } else {
        base.saturating_sub(n.unsigned_abs())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_end_forms_parse() {
        assert_eq!(Offset::parse("e"), Offset::End(0));
        assert_eq!(Offset::parse("e+1"), Offset::End(1));
        assert_eq!(Offset::parse("e-2"), Offset::End(-2));
    }

    #[test]
    fn the_start_forms_parse_under_both_spellings() {
        assert_eq!(Offset::parse("s+1"), Offset::Start(1));
        assert_eq!(Offset::parse("b+1"), Offset::Start(1));
        assert_eq!(Offset::parse("s"), Offset::Start(0));
    }

    #[test]
    fn a_bare_number_is_a_line_offset() {
        assert_eq!(Offset::parse("3"), Offset::Line(3));
        assert_eq!(Offset::parse("+2"), Offset::Line(2));
        assert_eq!(Offset::parse("-1"), Offset::Line(-1));
        assert_eq!(Offset::parse("+"), Offset::Line(1));
        assert!(Offset::parse("+2").is_line());
    }

    #[test]
    fn nothing_and_nonsense_are_both_no_offset() {
        assert_eq!(Offset::parse(""), Offset::None);
        assert_eq!(Offset::parse("zzz"), Offset::None);
    }

    #[test]
    fn a_bare_e_lands_on_the_last_character_of_the_match() {
        // Match covering columns 4..7 ("foo").
        assert_eq!(Offset::End(0).apply(4, 7), Some(6));
        assert_eq!(Offset::End(1).apply(4, 7), Some(7));
        assert_eq!(Offset::Start(0).apply(4, 7), Some(4));
        assert_eq!(Offset::Start(2).apply(4, 7), Some(6));
        assert_eq!(Offset::None.apply(4, 7), Some(4));
        assert_eq!(Offset::Line(1).apply(4, 7), None);
    }

    #[test]
    fn offsets_clamp_instead_of_underflowing() {
        assert_eq!(Offset::Start(-5).apply(2, 4), Some(0));
        assert_eq!(Offset::End(0).apply(0, 0), Some(0));
    }
}
