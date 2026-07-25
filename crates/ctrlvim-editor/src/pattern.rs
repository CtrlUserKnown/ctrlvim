//! Vim search patterns — the small part of `regexp.c` ctrlvim actually needs.
//!
//! Vim's "magic" regex flavor escapes the grouping/quantifier metacharacters
//! (`\(`, `\+`, `\|`, `\{`) — the reverse of PCRE — so a pattern typed at `/`
//! or in `:s` has to be translated before the `regex` crate sees it.
//!
//! This lives outside [`crate::session`] because searching is no longer only a
//! cursor concern: `:vimgrep` matches the same patterns across files on the
//! host's side of the filesystem boundary (see [`crate::quickfix::Matcher`]).

use regex::Regex;

/// Compile a Vim pattern into a [`Regex`].
pub fn compile(pat: &str) -> Result<Regex, regex::Error> {
    compile_opts(pat, false)
}

/// Compile a Vim pattern into a [`Regex`], optionally case-insensitive
/// (`'ignorecase'`).
pub fn compile_opts(pat: &str, ignorecase: bool) -> Result<Regex, regex::Error> {
    let translated = to_regex(pat);
    let src = if ignorecase { format!("(?i){translated}") } else { translated };
    Regex::new(&src)
}

/// Translate a Vim "magic" regex into Rust `regex` syntax: unescape Vim's
/// escaped metacharacters, escape the ones Vim treats as literal, and map
/// `\<`/`\>` (word boundaries) to `\b`.
pub fn to_regex(pat: &str) -> String {
    let mut out = String::new();
    let mut chars = pat.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('(') => out.push('('),
                Some(')') => out.push(')'),
                Some('+') => out.push('+'),
                Some('?') | Some('=') => out.push('?'),
                Some('|') => out.push('|'),
                Some('{') => out.push('{'),
                Some('}') => out.push('}'),
                Some('<') | Some('>') => out.push_str("\\b"),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            match c {
                // Literal in Vim magic mode → escape for PCRE.
                '(' | ')' | '+' | '?' | '|' | '{' | '}' => {
                    out.push('\\');
                    out.push(c);
                }
                _ => out.push(c),
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flips_vims_escaping_convention() {
        // Escaped in Vim = special in PCRE.
        assert_eq!(to_regex(r"\(ab\)\+"), r"(ab)+");
        assert_eq!(to_regex(r"foo\|bar"), "foo|bar");
        // Bare in Vim = literal, so PCRE needs the escape.
        assert_eq!(to_regex("f(x)"), r"f\(x\)");
        assert_eq!(to_regex("a+b"), r"a\+b");
    }

    #[test]
    fn maps_word_boundaries() {
        assert_eq!(to_regex(r"\<fn\>"), r"\bfn\b");
        let re = compile(r"\<fn\>").unwrap();
        assert!(re.is_match("fn main"));
        assert!(!re.is_match("effner"));
    }

    #[test]
    fn ignorecase_is_opt_in() {
        assert!(!compile("Fn").unwrap().is_match("fn"));
        assert!(compile_opts("Fn", true).unwrap().is_match("fn"));
    }

    #[test]
    fn a_trailing_backslash_is_not_a_panic() {
        assert_eq!(to_regex("ab\\"), "ab\\");
        // It is an invalid regex, which surfaces as an error rather than a panic.
        assert!(compile("ab\\").is_err());
    }
}
