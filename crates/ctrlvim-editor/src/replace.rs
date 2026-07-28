//! Project-wide find & replace — the search-and-substitute engine behind the
//! interactive replace panel.
//!
//! `:s` ([`crate::session::Session::ex_substitute`]) already substitutes inside
//! the current buffer. This module is the same operation lifted to *many files
//! at once*, and split so the engine keeps the semantics while the host keeps
//! the filesystem — the same seam [`crate::quickfix::Matcher`] draws for
//! `:vimgrep`. A [`ReplacePlan`] compiles the Vim pattern and the Vim
//! replacement once; the host walks the project, hands each file's contents to
//! [`ReplacePlan::hits_in`], and later asks the plan to produce the rewritten
//! text it should write back.
//!
//! Keeping it pure also makes the *preview* honest: the "after" line the panel
//! shows is produced by the very function that performs the edit, so what the
//! user reviews is exactly what lands on disk.

use std::path::{Path, PathBuf};

use ctrlvim_regex::{Regex, Replacement};

/// One matching line: where it is, what it says now, and what it would say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplaceHit {
    pub path: PathBuf,
    /// 0-based line, as everywhere else in the engine.
    pub line: usize,
    /// 0-based *character* column of the first match on the line.
    pub col: usize,
    /// The line as it stands.
    pub text: String,
    /// The line as it would stand after replacing every match on it.
    pub preview: String,
    /// How many matches this line holds.
    pub count: usize,
}

/// A compiled find & replace: a Vim pattern plus its replacement.
///
/// Both halves go through the same engine `:s` uses — [`crate::pattern`] for
/// the pattern's "magic" flavor, [`Replacement`] for `\1`/`&`/`\u` in the
/// replacement — so a pattern that works at `:s` works here unchanged.
pub struct ReplacePlan {
    re: Regex,
    replacement: Replacement,
}

impl ReplacePlan {
    /// Compile `pattern` and `replacement`; `Err` carries a user-facing message.
    ///
    /// An empty pattern is rejected rather than treated as "match everywhere":
    /// the panel calls this on every keystroke, and a zero-width match against
    /// the whole project is never what was meant.
    pub fn new(pattern: &str, replacement: &str, ignorecase: bool) -> Result<ReplacePlan, String> {
        if pattern.is_empty() {
            return Err("E35: No previous regular expression".into());
        }
        let re = crate::pattern::compile_opts(pattern, ignorecase)
            .map_err(|e| format!("E486: invalid pattern: {e}"))?;
        Ok(ReplacePlan { re, replacement: Replacement::parse(replacement, "") })
    }

    /// Every match on `line`, as `(start, end)` char columns.
    ///
    /// Empty matches are skipped for the same reason `:s` skips them — a
    /// zero-width hit has nothing to preview and nothing to replace.
    pub fn match_cols(&self, line: &str) -> Vec<(usize, usize)> {
        self.re
            .find_iter(line)
            .filter(|m| !m.is_empty())
            .map(|m| (m.start_char(), m.end_char()))
            .collect()
    }

    /// The line with every match replaced, or `None` when nothing matched.
    pub fn apply_line(&self, line: &str) -> Option<String> {
        self.re.is_match(line).then(|| self.re.replace_all(line, &self.replacement))
    }

    /// The line with only the match starting at char column `col` replaced —
    /// what accepting a single occurrence does.
    ///
    /// Returns `None` when no match starts exactly there, so a stale hit (the
    /// file changed under us) is a no-op rather than a wrong edit.
    pub fn apply_match_at(&self, line: &str, col: usize) -> Option<String> {
        // Captures rather than plain matches, so `\1` in the replacement still
        // resolves for the one occurrence being accepted.
        let caps = self.re.captures_iter(line).find(|c| {
            let m = c.get(0).expect("group 0 always matches");
            m.start_char() == col && !m.is_empty()
        })?;
        let m = caps.get(0).expect("group 0 always matches");
        let mut out = String::with_capacity(line.len());
        out.push_str(&line[..m.start()]);
        caps.expand(&self.replacement, &mut out);
        out.push_str(&line[m.end()..]);
        Some(out)
    }

    /// Collect one hit per matching line of `text` — the per-file half of the
    /// search, kept pure so the caller supplies the contents it read.
    pub fn hits_in(&self, path: &Path, text: &str) -> Vec<ReplaceHit> {
        text.lines()
            .enumerate()
            .filter_map(|(i, line)| {
                let cols = self.match_cols(line);
                let (col, _) = *cols.first()?;
                Some(ReplaceHit {
                    path: path.to_path_buf(),
                    line: i,
                    col,
                    text: line.to_string(),
                    preview: self.apply_line(line)?,
                    count: cols.len(),
                })
            })
            .collect()
    }

    /// Rewrite every line of `lines`, returning the new lines and how many
    /// *matches* were replaced — what accepting all occurrences in one file
    /// does.
    ///
    /// Returns `None` when the file has no matches, so the caller can skip the
    /// write entirely rather than rewriting a file byte-for-byte identically.
    pub fn apply_lines(&self, lines: &[String]) -> Option<(Vec<String>, usize)> {
        let mut replaced = 0usize;
        let mut out = Vec::with_capacity(lines.len());
        for line in lines {
            let hits = self.match_cols(line).len();
            if hits > 0 {
                replaced += hits;
                // A replacement containing `\n` splits one line into several,
                // exactly as `:s/…/\r/` does.
                let new = self.apply_line(line).unwrap_or_else(|| line.clone());
                out.extend(new.split('\n').map(str::to_string));
            } else {
                out.push(line.clone());
            }
        }
        (replaced > 0).then_some((out, replaced))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(pat: &str, rep: &str) -> ReplacePlan {
        ReplacePlan::new(pat, rep, false).unwrap()
    }

    #[test]
    fn hits_carry_the_before_and_after_of_each_line() {
        let p = plan("foo", "bar");
        let hits = p.hits_in(Path::new("a.rs"), "let foo = 1;\nnothing\nfoo(foo);\n");
        assert_eq!(hits.len(), 2, "one hit per matching line, not per match");
        assert_eq!((hits[0].line, hits[0].col), (0, 4));
        assert_eq!(hits[0].preview, "let bar = 1;");
        assert_eq!(hits[1].count, 2, "both matches on the line are counted");
        assert_eq!(hits[1].preview, "bar(bar);", "preview replaces all of them");
    }

    #[test]
    fn accepting_one_occurrence_leaves_the_others_alone() {
        let p = plan("foo", "bar");
        let line = "foo(foo, foo)";
        assert_eq!(p.apply_match_at(line, 0).unwrap(), "bar(foo, foo)");
        assert_eq!(p.apply_match_at(line, 4).unwrap(), "foo(bar, foo)");
        assert_eq!(p.apply_match_at(line, 9).unwrap(), "foo(foo, bar)");
        assert!(p.apply_match_at(line, 2).is_none(), "no match starts there");
    }

    #[test]
    fn columns_are_characters_not_bytes() {
        let p = plan("x", "y");
        let hits = p.hits_in(Path::new("a.rs"), "// 🌍 x");
        assert_eq!(hits[0].col, 5, "char column past the emoji");
        // And a single-occurrence accept addressed by that column still lands.
        assert_eq!(p.apply_match_at("// 🌍 x", 5).unwrap(), "// 🌍 y");
    }

    #[test]
    fn the_pattern_is_vims_magic_flavor() {
        // `\<`/`\>` are word boundaries, and a bare `(` is a literal.
        let p = plan(r"\<fn\>", "func");
        let hits = p.hits_in(Path::new("a.rs"), "fn one() {}\nlet fnord = 1;\n");
        assert_eq!(hits.len(), 1, "`fnord` is not a word match");
        assert_eq!(hits[0].preview, "func one() {}");
    }

    #[test]
    fn the_replacement_understands_capture_groups() {
        let p = plan(r"\(\w\+\)_old", r"\1_new");
        assert_eq!(p.apply_line("call foo_old()").unwrap(), "call foo_new()");
        // `&` is the whole match.
        assert_eq!(plan("foo", "[&]").apply_line("a foo b").unwrap(), "a [foo] b");
        // A literal `$` survives rather than being read as a capture ref.
        assert_eq!(plan("x", "$y").apply_line("x").unwrap(), "$y");
    }

    #[test]
    fn applying_a_whole_file_counts_matches_not_lines() {
        let p = plan("foo", "bar");
        let lines: Vec<String> =
            ["foo foo", "nothing", "foo"].iter().map(|s| s.to_string()).collect();
        let (out, n) = p.apply_lines(&lines).unwrap();
        assert_eq!(n, 3, "three matches across two lines");
        assert_eq!(out, vec!["bar bar", "nothing", "bar"]);
    }

    #[test]
    fn a_replacement_newline_splits_the_line() {
        let p = plan(", ", r"\r");
        let lines = vec!["a, b".to_string()];
        let (out, _) = p.apply_lines(&lines).unwrap();
        assert_eq!(out, vec!["a", "b"]);
    }

    #[test]
    fn a_file_with_no_matches_is_left_untouched() {
        let p = plan("foo", "bar");
        assert!(p.apply_lines(&["nothing here".to_string()]).is_none());
        assert!(p.apply_line("nothing here").is_none());
        assert!(p.hits_in(Path::new("a.rs"), "nothing here").is_empty());
    }

    #[test]
    fn ignorecase_is_opt_in() {
        assert!(ReplacePlan::new("FOO", "bar", false).unwrap().apply_line("foo").is_none());
        assert_eq!(
            ReplacePlan::new("FOO", "bar", true).unwrap().apply_line("foo").unwrap(),
            "bar"
        );
    }

    #[test]
    fn empty_and_invalid_patterns_are_errors_not_panics() {
        assert!(ReplacePlan::new("", "x", false).is_err());
        assert!(ReplacePlan::new(r"\(unclosed", "x", false).is_err());
        // A bare `[` is a literal in Vim, so this one is a valid pattern.
        assert!(ReplacePlan::new("[unclosed", "x", false).is_ok());
    }

    #[test]
    fn zero_width_matches_are_skipped() {
        // `x*` matches empty at every position; only the real `x` should count.
        let p = plan("x*", "-");
        assert_eq!(p.match_cols("axb"), vec![(1, 2)]);
    }
}
