//! Vim search patterns — the editor's front door to [`ctrlvim_regex`].
//!
//! This module used to translate Vim's "magic" syntax into the `regex` crate's
//! and hand the result to a DFA. It no longer translates anything: patterns go
//! to a native Vim engine that speaks the syntax directly, which is what makes
//! `\1`, `\@<=`, `\zs` and `\{-}` work instead of merely parse.
//!
//! What is left here is policy rather than semantics — deciding *how* to
//! compile a pattern for this editor:
//!
//! - `'ignorecase'` and `'smartcase'` are resolved in one place
//!   ([`effective_ignorecase`]) so `/`, `:s`, `:g` and the find panel cannot
//!   drift apart on what "case sensitive" means;
//! - a [`Context`] is built per line, so the positional atoms (`\%23l`,
//!   `\%>3v`) have real data to compare against.
//!
//! It lives outside [`crate::session`] because searching is no longer only a
//! cursor concern: `:vimgrep` matches the same patterns across files on the
//! host's side of the filesystem boundary (see [`crate::quickfix::Matcher`]).

use ctrlvim_options::GlobalOptions;
pub use ctrlvim_regex::{Context, Error, Match, Regex, Replacement};

/// Compile a Vim pattern, case-sensitively.
pub fn compile(pat: &str) -> Result<Regex, Error> {
    compile_opts(pat, false)
}

/// Compile a Vim pattern, optionally case-insensitive.
///
/// A `\c` or `\C` inside the pattern still overrides the flag — that is what
/// those atoms are for, and the engine applies them itself.
pub fn compile_opts(pat: &str, ignorecase: bool) -> Result<Regex, Error> {
    Regex::with_ignorecase(pat, ignorecase)
}

/// Compile a pattern the way the editor's options say to.
///
/// `force` is the `:s///i` flag, which turns case-insensitivity on regardless
/// of the options.
pub fn compile_with(pat: &str, opts: &GlobalOptions, force: bool) -> Result<Regex, Error> {
    compile_opts(pat, force || effective_ignorecase(pat, opts))
}

/// Resolve `'ignorecase'` against `'smartcase'`.
///
/// `'smartcase'` only ever *removes* case-insensitivity, and only when the user
/// typed an uppercase letter — the signal that they meant it. It has no effect
/// unless `'ignorecase'` is on, which is the rule people forget.
pub fn effective_ignorecase(pat: &str, opts: &GlobalOptions) -> bool {
    if !opts.ignorecase {
        return false;
    }
    if opts.smartcase && has_unescaped_upper(pat) {
        return false;
    }
    true
}

/// Whether the pattern contains an uppercase letter that the user actually
/// typed, rather than one that is part of an escape like `\S` or `\%V`.
fn has_unescaped_upper(pat: &str) -> bool {
    let mut chars = pat.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            // Skip the escaped character: `\D` is a class, not a capital D.
            chars.next();
            continue;
        }
        if c.is_uppercase() {
            return true;
        }
    }
    false
}

/// A [`Context`] for matching inside one buffer line.
///
/// Supplying it is what lets `\%23l` and `\%>10v` mean anything; without it
/// those atoms stop constraining rather than silently failing.
pub fn line_context(line: usize, line_count: usize, tabstop: i64) -> Context {
    Context {
        // Vim's `\%23l` counts from one.
        line: Some(line + 1),
        first_line: Some(line == 0),
        last_line: Some(line + 1 >= line_count),
        cursor_col: None,
        visual: None,
        tabstop: tabstop.max(1) as usize,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pattern_flavor_is_vims_magic() {
        // Escaped in Vim = an operator.
        assert!(compile(r"\(ab\)\+").expect("compiles").is_match("abab"));
        assert!(compile(r"foo\|bar").expect("compiles").is_match("bar"));
        // Bare in Vim = a literal.
        assert!(compile("f(x)").expect("compiles").is_match("f(x)"));
        assert!(compile("a+b").expect("compiles").is_match("a+b"));
    }

    #[test]
    fn word_boundaries_match_whole_words() {
        let re = compile(r"\<fn\>").expect("compiles");
        assert!(re.is_match("fn main"));
        assert!(!re.is_match("effner"));
    }

    #[test]
    fn ignorecase_is_opt_in() {
        assert!(!compile("Fn").expect("compiles").is_match("fn"));
        assert!(compile_opts("Fn", true).expect("compiles").is_match("fn"));
    }

    #[test]
    fn a_trailing_backslash_is_an_error_not_a_panic() {
        assert!(compile("ab\\").is_err());
    }

    /// Options with just the two search flags set.
    fn opts(ignorecase: bool, smartcase: bool) -> GlobalOptions {
        GlobalOptions { ignorecase, smartcase, ..GlobalOptions::default() }
    }

    #[test]
    fn smartcase_only_bites_when_ignorecase_is_on() {
        // 'smartcase' alone does nothing.
        let off = opts(false, true);
        assert!(!effective_ignorecase("Foo", &off));
        assert!(!effective_ignorecase("foo", &off));

        let on = opts(true, true);
        // All lowercase: still insensitive.
        assert!(effective_ignorecase("foo", &on));
        // A typed capital means the user meant it.
        assert!(!effective_ignorecase("Foo", &on));
    }

    #[test]
    fn smartcase_ignores_capitals_that_belong_to_an_escape() {
        let o = opts(true, true);
        // `\S` is a class, not a capital the user typed.
        assert!(effective_ignorecase(r"\Sfoo", &o));
        assert!(!effective_ignorecase(r"\SFoo", &o));
    }

    #[test]
    fn the_substitute_flag_overrides_the_options() {
        let re = compile_with("Foo", &opts(false, false), true).expect("compiles");
        assert!(re.is_match("foo"));
    }

    #[test]
    fn a_line_context_makes_positional_atoms_real() {
        let re = compile(r"\%2lfoo").expect("compiles");
        // Line index 1 is line number 2.
        let ctx = line_context(1, 5, 8);
        assert!(re.captures_ctx("foo", &ctx).is_some());
        let ctx = line_context(2, 5, 8);
        assert!(re.captures_ctx("foo", &ctx).is_none());
    }
}
