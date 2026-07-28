//! A Vim regular expression engine — the replacement for `regexp.c`.
//!
//! ctrlvim used to translate Vim patterns into the `regex` crate's syntax. That
//! worked for the easy nine tenths and hit a wall on the rest: `regex` compiles
//! to a DFA, and a DFA cannot evaluate a backreference or run a lookaround at
//! all. `\1`, `\@=`, `\@<=`, `\zs` and `\ze` were not missing features, they
//! were unreachable ones. This crate exists to make them reachable.
//!
//! The pipeline is the same one `regexp.c` uses:
//!
//! ```text
//! pattern → parse (magic levels) → AST → compile → program → backtracking VM
//! ```
//!
//! Each stage is its own module: [`parse`], [`program`], [`vm`]. Two extras sit
//! alongside because they are part of "Vim patterns" as users experience them
//! even though they are not the matcher: [`Replacement`] (the `:s` replacement
//! language, including `\u`/`\U` case folding) and [`Offset`] (the `/pat/e+1`
//! suffix).
//!
//! # Positions
//!
//! Every public [`Match`] reports **byte** offsets from [`Match::start`] and
//! [`Match::end`], so it slices a `&str` directly, and **character** columns
//! from [`Match::start_char`] and [`Match::end_char`], which is what the editor
//! actually stores. Both are computed once, at the boundary; the VM works in
//! character indices throughout.
//!
//! ```
//! use ctrlvim_regex::Regex;
//!
//! // A backreference — impossible under the old DFA.
//! let re = Regex::new(r"\<\(\w\+\) \1\>").expect("valid pattern");
//! let m = re.find("the the end").expect("matches");
//! assert_eq!(&"the the end"[m.start()..m.end()], "the the");
//! ```

pub mod class;
pub mod offset;
pub mod parse;
pub mod program;
pub mod replace;
pub mod vm;

pub use offset::Offset;
pub use parse::{Error, Magic};
pub use replace::Replacement;
pub use vm::{Context, Limits};

use program::Program;
use vm::Input;

/// How a pattern should be compiled.
#[derive(Debug, Clone)]
pub struct Options {
    /// `'ignorecase'`. A `\c` or `\C` in the pattern overrides it, which is the
    /// entire purpose of those atoms.
    pub ignorecase: bool,
    /// Starting magic level; `\v`/`\m`/`\M`/`\V` change it mid-pattern.
    pub magic: Magic,
    /// What `~` in a *pattern* stands for — the previous `:s` replacement.
    pub last_sub: String,
    pub limits: Limits,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            ignorecase: false,
            magic: Magic::Magic,
            last_sub: String::new(),
            limits: Limits::default(),
        }
    }
}

/// A compiled Vim pattern.
#[derive(Debug, Clone)]
pub struct Regex {
    prog: Program,
    ignorecase: bool,
    limits: Limits,
    pattern: String,
}

impl Regex {
    /// Compile `pat` at the default magic level, case-sensitively.
    pub fn new(pat: &str) -> Result<Regex, Error> {
        Regex::with_options(pat, &Options::default())
    }

    /// Compile `pat`, honoring `'ignorecase'`.
    pub fn with_ignorecase(pat: &str, ignorecase: bool) -> Result<Regex, Error> {
        Regex::with_options(pat, &Options { ignorecase, ..Options::default() })
    }

    pub fn with_options(pat: &str, opts: &Options) -> Result<Regex, Error> {
        let parsed = parse::parse(pat, opts.magic, &opts.last_sub)?;
        let prog = program::compile(&parsed.ast, parsed.groups);
        Ok(Regex {
            prog,
            ignorecase: parsed.force_ignorecase.unwrap_or(opts.ignorecase),
            limits: opts.limits,
            pattern: pat.to_string(),
        })
    }

    /// The pattern this was compiled from.
    pub fn as_str(&self) -> &str {
        &self.pattern
    }

    /// Number of capturing groups, excluding the whole match.
    pub fn groups(&self) -> usize {
        self.prog.groups
    }

    pub fn is_match(&self, text: &str) -> bool {
        self.find(text).is_some()
    }

    /// The leftmost match, if any.
    pub fn find<'t>(&self, text: &'t str) -> Option<Match<'t>> {
        self.captures(text).and_then(|c| c.get(0))
    }

    /// The leftmost match at or after byte offset `from`.
    pub fn find_at<'t>(&self, text: &'t str, from: usize) -> Option<Match<'t>> {
        let input = Input::new(text);
        let start = input.char_of_byte(from);
        self.exec(&input, start, &Context::new()).and_then(|c| c.get(0))
    }

    pub fn captures<'t>(&self, text: &'t str) -> Option<Captures<'t>> {
        let input = Input::new(text);
        self.exec(&input, 0, &Context::new())
    }

    /// Captures for the leftmost match, evaluated with `ctx` — which is how a
    /// caller that knows the line number makes `\%23l` mean something.
    pub fn captures_ctx<'t>(&self, text: &'t str, ctx: &Context) -> Option<Captures<'t>> {
        let input = Input::new(text);
        self.exec(&input, 0, ctx)
    }

    /// Every non-overlapping match, left to right.
    pub fn find_iter<'r, 't>(&'r self, text: &'t str) -> Matches<'r, 't> {
        Matches { inner: self.captures_iter(text) }
    }

    pub fn captures_iter<'r, 't>(&'r self, text: &'t str) -> CaptureMatches<'r, 't> {
        self.captures_iter_ctx(text, Context::new())
    }

    pub fn captures_iter_ctx<'r, 't>(&'r self, text: &'t str, ctx: Context) -> CaptureMatches<'r, 't> {
        CaptureMatches { re: self, input: Input::new(text), pos: 0, done: false, ctx }
    }

    /// Every match, evaluated with `ctx`.
    pub fn find_iter_ctx<'r, 't>(&'r self, text: &'t str, ctx: Context) -> Matches<'r, 't> {
        Matches { inner: self.captures_iter_ctx(text, ctx) }
    }

    /// Replace the first match.
    pub fn replace(&self, text: &str, rep: &Replacement) -> String {
        self.replace_n(text, rep, 1)
    }

    /// Replace every match.
    pub fn replace_all(&self, text: &str, rep: &Replacement) -> String {
        self.replace_n(text, rep, usize::MAX)
    }

    /// Replace up to `limit` matches, returning the rewritten text.
    pub fn replace_n(&self, text: &str, rep: &Replacement, limit: usize) -> String {
        self.replace_n_ctx(text, rep, limit, Context::new())
    }

    /// Replace up to `limit` matches, evaluated with `ctx` — the form `:s` uses
    /// so that a pattern like `\%23l` sees the line it is actually on.
    pub fn replace_n_ctx(
        &self,
        text: &str,
        rep: &Replacement,
        limit: usize,
        ctx: Context,
    ) -> String {
        let mut out = String::with_capacity(text.len());
        let mut last = 0usize;
        for (i, caps) in self.captures_iter_ctx(text, ctx).enumerate() {
            if i >= limit {
                break;
            }
            let m = caps.get(0).expect("group 0 always matches");
            out.push_str(&text[last..m.start()]);
            rep.expand(&caps, &mut out);
            last = m.end();
        }
        out.push_str(&text[last..]);
        out
    }

    /// How many matches `text` holds.
    pub fn count(&self, text: &str) -> usize {
        self.find_iter(text).count()
    }

    fn exec<'t>(&self, input: &Input<'t>, from: usize, ctx: &Context) -> Option<Captures<'t>> {
        let m = vm::find_at(&self.prog, input, from, ctx, self.ignorecase, self.limits)?;
        Some(Captures::build(input, m))
    }
}

/// One match: a span of the searched text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Match<'t> {
    text: &'t str,
    start: usize,
    end: usize,
    start_char: usize,
    end_char: usize,
}

impl<'t> Match<'t> {
    /// Byte offset of the first character, for slicing the original `&str`.
    pub fn start(&self) -> usize {
        self.start
    }

    /// Byte offset one past the last character.
    pub fn end(&self) -> usize {
        self.end
    }

    /// Character column of the first character — what the editor stores.
    pub fn start_char(&self) -> usize {
        self.start_char
    }

    /// Character column one past the last character.
    pub fn end_char(&self) -> usize {
        self.end_char
    }

    pub fn as_str(&self) -> &'t str {
        &self.text[self.start..self.end]
    }

    /// Whether the match consumed nothing. Zero-width matches are legitimate
    /// (`/^/`, `/x*/`) but callers usually want to skip them.
    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }
}

/// The groups of one match.
#[derive(Debug, Clone)]
pub struct Captures<'t> {
    text: &'t str,
    /// Byte and character offsets per slot, parallel to the VM's slot list.
    bytes: Vec<Option<usize>>,
    chars: Vec<Option<usize>>,
}

impl<'t> Captures<'t> {
    fn build(input: &Input<'t>, m: vm::Match) -> Captures<'t> {
        let chars = m.slots;
        let bytes = chars.iter().map(|s| s.map(|c| input.byte_of(c))).collect();
        Captures { text: input.text, bytes, chars }
    }

    /// Group `i`, or `None` if it did not participate in the match. Group 0 is
    /// the whole match.
    pub fn get(&self, i: usize) -> Option<Match<'t>> {
        let (s, e) = (self.bytes.get(i * 2)?, self.bytes.get(i * 2 + 1)?);
        let (sc, ec) = (self.chars.get(i * 2)?, self.chars.get(i * 2 + 1)?);
        Some(Match {
            text: self.text,
            start: (*s)?,
            end: (*e)?,
            start_char: (*sc)?,
            end_char: (*ec)?,
        })
    }

    /// Number of groups including group 0.
    pub fn len(&self) -> usize {
        self.bytes.len() / 2
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Expand `rep` against these captures.
    pub fn expand(&self, rep: &Replacement, out: &mut String) {
        rep.expand(self, out);
    }
}

/// Iterator over successive matches.
pub struct Matches<'r, 't> {
    inner: CaptureMatches<'r, 't>,
}

impl<'t> Iterator for Matches<'_, 't> {
    type Item = Match<'t>;

    fn next(&mut self) -> Option<Match<'t>> {
        self.inner.next().and_then(|c| c.get(0))
    }
}

/// Iterator over successive matches, with their groups.
pub struct CaptureMatches<'r, 't> {
    re: &'r Regex,
    input: Input<'t>,
    pos: usize,
    done: bool,
    ctx: Context,
}

impl<'t> Iterator for CaptureMatches<'_, 't> {
    type Item = Captures<'t>;

    fn next(&mut self) -> Option<Captures<'t>> {
        if self.done || self.pos > self.input.len() {
            return None;
        }
        let caps = self.re.exec(&self.input, self.pos, &self.ctx)?;
        let m = caps.get(0).expect("group 0 always matches");
        // A zero-width match would otherwise be found forever; step past it.
        self.pos = if m.end_char() == m.start_char() { m.end_char() + 1 } else { m.end_char() };
        if self.pos > self.input.len() {
            self.done = true;
        }
        Some(caps)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_magic_flavor_is_vims_not_pcres() {
        // Bare parens and plus are literals at the default magic level.
        assert!(Regex::new("f(x)").expect("compiles").is_match("f(x)"));
        assert!(Regex::new("a+b").expect("compiles").is_match("a+b"));
        // Backslashed, they are operators.
        assert!(Regex::new(r"\(ab\)\+").expect("compiles").is_match("abab"));
    }

    #[test]
    fn very_magic_flips_the_convention() {
        let re = Regex::with_options(
            r"(ab)+",
            &Options { magic: Magic::Very, ..Options::default() },
        )
        .expect("compiles");
        assert!(re.is_match("abab"));
        // The same effect written inline.
        assert!(Regex::new(r"\v(ab)+").expect("compiles").is_match("abab"));
    }

    #[test]
    fn very_nomagic_makes_everything_literal() {
        let re = Regex::new(r"\Va.c").expect("compiles");
        assert!(re.is_match("xa.cx"));
        assert!(!re.is_match("abc"));
    }

    #[test]
    fn ignorecase_is_opt_in_and_the_pattern_can_override_it() {
        assert!(!Regex::new("Fn").expect("compiles").is_match("fn"));
        assert!(Regex::with_ignorecase("Fn", true).expect("compiles").is_match("fn"));
        // `\c` wins over the option being off.
        assert!(Regex::new(r"\cFn").expect("compiles").is_match("fn"));
        // `\C` wins over the option being on.
        assert!(!Regex::with_ignorecase(r"\CFn", true).expect("compiles").is_match("fn"));
    }

    #[test]
    fn matches_report_both_byte_and_character_positions() {
        let re = Regex::new("x").expect("compiles");
        let m = re.find("日本語x").expect("matches");
        // Three 3-byte characters precede the target.
        assert_eq!(m.start(), 9);
        assert_eq!(m.start_char(), 3);
        assert_eq!(m.as_str(), "x");
    }

    #[test]
    fn find_iter_walks_every_match_and_survives_zero_width_ones() {
        let re = Regex::new("a").expect("compiles");
        assert_eq!(re.find_iter("banana").count(), 3);
        // `x*` matches empty at every position rather than looping forever.
        let re = Regex::new("x*").expect("compiles");
        assert_eq!(re.find_iter("ab").count(), 3);
    }

    #[test]
    fn replace_honors_the_limit() {
        let re = Regex::new("a").expect("compiles");
        let rep = Replacement::parse("X", "");
        assert_eq!(re.replace("banana", &rep), "bXnana");
        assert_eq!(re.replace_all("banana", &rep), "bXnXnX");
    }

    #[test]
    fn an_invalid_pattern_is_an_error_not_a_panic() {
        assert!(Regex::new(r"\(unclosed").is_err());
        assert!(Regex::new("ab\\").is_err());
        assert!(Regex::new(r"\1").is_err());
    }

    #[test]
    fn the_features_the_dfa_could_not_express_all_work() {
        // Backreference.
        assert!(Regex::new(r"\(ab\)\1").expect("compiles").is_match("abab"));
        // Lookahead and lookbehind.
        assert!(Regex::new(r"foo\(bar\)\@=").expect("compiles").is_match("foobar"));
        assert!(Regex::new(r"\(foo\)\@<=bar").expect("compiles").is_match("foobar"));
        // Match-boundary markers.
        let m = Regex::new(r"foo\zsbar").expect("compiles").find("foobar").expect("matches");
        assert_eq!(m.as_str(), "bar");
        // Non-greedy brace.
        let m = Regex::new(r"a.\{-}b").expect("compiles").find("axxbxxb").expect("matches");
        assert_eq!(m.as_str(), "axxb");
    }

    #[test]
    fn captures_expose_groups_by_index() {
        let re = Regex::new(r"\(\w\+\)@\(\w\+\)").expect("compiles");
        let caps = re.captures("mail: user@host!").expect("matches");
        assert_eq!(caps.get(0).expect("whole match").as_str(), "user@host");
        assert_eq!(caps.get(1).expect("group 1").as_str(), "user");
        assert_eq!(caps.get(2).expect("group 2").as_str(), "host");
        assert_eq!(caps.len(), 3);
    }

    #[test]
    fn a_group_that_did_not_participate_reports_none() {
        let re = Regex::new(r"a\(x\)\?b").expect("compiles");
        let caps = re.captures("ab").expect("matches");
        assert!(caps.get(1).is_none());
    }
}
