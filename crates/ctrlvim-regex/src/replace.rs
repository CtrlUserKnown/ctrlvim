//! Vim replacement strings — the `sub_expand` half of `:s`.
//!
//! The replacement is its own little language and it is *not* the pattern
//! language: `\1` refers to a group, `&` is the whole match, `~` is the
//! previous replacement, and `\u`/`\U`/`\l`/`\L`/`\e`/`\E` change the case of
//! what follows. Those case escapes are the reason this is compiled rather than
//! substituted textually — `\u\1` has to uppercase the first character of
//! whatever group 1 turned out to be, which no amount of string splicing knows
//! in advance.

use crate::Captures;

/// A case transformation requested by `\u`, `\U`, `\l` or `\L`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Case {
    Upper,
    Lower,
}

#[derive(Debug, Clone, PartialEq)]
enum Piece {
    Literal(String),
    /// `\1`–`\9`, and `\0`/`&` for the whole match.
    Group(usize),
    /// `~` — the previous replacement text.
    PrevSub,
    /// `\u` / `\l` — applies to one character.
    CaseOne(Case),
    /// `\U` / `\L` — applies until `\e` or `\E`.
    CaseSpan(Case),
    /// `\e` / `\E`.
    CaseEnd,
}

/// A compiled replacement.
#[derive(Debug, Clone, PartialEq)]
pub struct Replacement {
    pieces: Vec<Piece>,
    /// Whether the source was the bare `~`, which means "reuse the last
    /// replacement wholesale".
    prev_sub: String,
}

impl Replacement {
    /// Parse a Vim replacement string.
    ///
    /// `prev` is the text `~` expands to. Passing an empty string leaves `~`
    /// standing for itself, which is what a first-ever `:s` should do.
    pub fn parse(rep: &str, prev: &str) -> Replacement {
        let mut pieces: Vec<Piece> = Vec::new();
        let mut lit = String::new();
        let mut chars = rep.chars().peekable();

        // Accumulating literals and flushing on demand keeps the piece list
        // short, which matters because expansion runs per match.
        macro_rules! flush {
            () => {
                if !lit.is_empty() {
                    pieces.push(Piece::Literal(std::mem::take(&mut lit)));
                }
            };
        }

        while let Some(c) = chars.next() {
            match c {
                '\\' => match chars.next() {
                    Some(d @ '0'..='9') => {
                        flush!();
                        pieces.push(Piece::Group(d as usize - '0' as usize));
                    }
                    // Vim's `\r` inserts a line break. `\n` officially inserts a
                    // NUL, which has no useful meaning in a Rust string, so it
                    // is treated as a line break too.
                    Some('r' | 'n') => lit.push('\n'),
                    Some('t') => lit.push('\t'),
                    Some('&') => lit.push('&'),
                    Some('~') => lit.push('~'),
                    Some('\\') => lit.push('\\'),
                    Some('u') => {
                        flush!();
                        pieces.push(Piece::CaseOne(Case::Upper));
                    }
                    Some('l') => {
                        flush!();
                        pieces.push(Piece::CaseOne(Case::Lower));
                    }
                    Some('U') => {
                        flush!();
                        pieces.push(Piece::CaseSpan(Case::Upper));
                    }
                    Some('L') => {
                        flush!();
                        pieces.push(Piece::CaseSpan(Case::Lower));
                    }
                    Some('e' | 'E') => {
                        flush!();
                        pieces.push(Piece::CaseEnd);
                    }
                    Some(other) => lit.push(other),
                    None => lit.push('\\'),
                },
                '&' => {
                    flush!();
                    pieces.push(Piece::Group(0));
                }
                // With no previous replacement to reuse, `~` stands for itself
                // rather than silently expanding to nothing.
                '~' if prev.is_empty() => lit.push('~'),
                '~' => {
                    flush!();
                    pieces.push(Piece::PrevSub);
                }
                _ => lit.push(c),
            }
        }
        if !lit.is_empty() {
            pieces.push(Piece::Literal(lit));
        }
        Replacement { pieces, prev_sub: prev.to_string() }
    }

    /// Whether the replacement inserts a line break, which the caller needs to
    /// know because one line becomes several.
    pub fn splits_lines(&self) -> bool {
        self.pieces.iter().any(|p| matches!(p, Piece::Literal(s) if s.contains('\n')))
    }

    /// Expand against `caps`, appending to `out`.
    pub fn expand(&self, caps: &Captures<'_>, out: &mut String) {
        let mut span: Option<Case> = None;
        let mut one: Option<Case> = None;

        for piece in &self.pieces {
            match piece {
                Piece::CaseOne(c) => one = Some(*c),
                Piece::CaseSpan(c) => span = Some(*c),
                Piece::CaseEnd => {
                    span = None;
                    one = None;
                }
                Piece::Literal(s) => push_cased(s, &mut one, span, out),
                Piece::PrevSub => {
                    let prev = self.prev_sub.clone();
                    push_cased(&prev, &mut one, span, out);
                }
                Piece::Group(n) => {
                    let text = caps.get(*n).map(|m| m.as_str()).unwrap_or("");
                    push_cased(text, &mut one, span, out);
                }
            }
        }
    }

    /// Expand into a fresh string.
    pub fn expand_str(&self, caps: &Captures<'_>) -> String {
        let mut out = String::new();
        self.expand(caps, &mut out);
        out
    }
}

/// Append `s`, honoring a pending one-character case change and any active
/// case span.
fn push_cased(s: &str, one: &mut Option<Case>, span: Option<Case>, out: &mut String) {
    let mut chars = s.chars();
    if let Some(c) = chars.next() {
        // `\u` applies to the first character only, and is spent even if the
        // group it precedes turned out to be empty of letters.
        let first = match one.take() {
            Some(Case::Upper) => c.to_uppercase().collect::<String>(),
            Some(Case::Lower) => c.to_lowercase().collect::<String>(),
            None => apply_span(c, span),
        };
        out.push_str(&first);
    } else {
        return;
    }
    for c in chars {
        out.push_str(&apply_span(c, span));
    }
}

fn apply_span(c: char, span: Option<Case>) -> String {
    match span {
        Some(Case::Upper) => c.to_uppercase().collect(),
        Some(Case::Lower) => c.to_lowercase().collect(),
        None => c.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Regex;

    fn sub(pat: &str, rep: &str, text: &str) -> String {
        let re = Regex::new(pat).expect("compiles");
        re.replace_all(text, &Replacement::parse(rep, ""))
    }

    #[test]
    fn groups_and_the_whole_match_expand() {
        assert_eq!(sub(r"\(a\)\(b\)", r"\2\1", "ab"), "ba");
        assert_eq!(sub("b", "[&]", "abc"), "a[b]c");
        assert_eq!(sub("b", r"[\0]", "abc"), "a[b]c");
    }

    #[test]
    fn an_escaped_ampersand_is_a_literal() {
        assert_eq!(sub("b", r"\&", "abc"), "a&c");
    }

    #[test]
    fn one_character_case_escapes_apply_to_the_next_character_only() {
        assert_eq!(sub(r"\(\w\+\)", r"\u\1", "hello"), "Hello");
        assert_eq!(sub(r"\(\w\+\)", r"\l\1", "HELLO"), "hELLO");
    }

    #[test]
    fn span_case_escapes_run_until_they_are_ended() {
        assert_eq!(sub(r"\(\w\+\)", r"\U\1", "hello"), "HELLO");
        assert_eq!(sub(r"\(\w\+\)", r"\U\1\E!", "hello"), "HELLO!");
        // `\e` ends a span the same way `\E` does.
        assert_eq!(sub(r"\(\w\)\(\w\+\)", r"\U\1\e\2", "hello"), "Hello");
    }

    #[test]
    fn a_newline_in_the_replacement_is_reported() {
        assert!(Replacement::parse(r"a\rb", "").splits_lines());
        assert!(!Replacement::parse(r"ab", "").splits_lines());
    }

    #[test]
    fn tilde_reuses_the_previous_replacement() {
        let re = Regex::new("b").expect("compiles");
        assert_eq!(re.replace_all("abc", &Replacement::parse("~", "XY")), "aXYc");
        // With no previous replacement it stands for itself.
        assert_eq!(re.replace_all("abc", &Replacement::parse("~", "")), "a~c");
    }

    #[test]
    fn a_trailing_backslash_is_a_literal_not_a_panic() {
        assert_eq!(sub("b", "x\\", "abc"), "ax\\c");
    }
}
