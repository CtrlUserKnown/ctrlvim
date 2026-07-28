//! The quickfix list — the replacement for `quickfix.c`'s list half.
//!
//! A quickfix list is a cursor over positions in files: compiler errors from
//! `:make`, matches from `:grep`/`:vimgrep`, and (later) LSP diagnostics all
//! land in the same structure, and `:cnext`/`:cprev` walk it.
//!
//! This module is deliberately pure data — no file I/O, no process handling —
//! so it unit-tests the way [`crate::motion`] does. Filling the list is the
//! caller's job; jumping to an item is the frontend's.

use std::path::PathBuf;

/// The severity of an entry, mapped from whatever produced it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QfKind {
    Error,
    Warning,
    Info,
    Note,
    /// A plain match with no severity (`:grep`, `:vimgrep`).
    Match,
}

impl QfKind {
    /// The single-character tag Vim shows in the quickfix window (`E`, `W`, …).
    pub fn tag(self) -> char {
        match self {
            QfKind::Error => 'E',
            QfKind::Warning => 'W',
            QfKind::Info => 'I',
            QfKind::Note => 'N',
            QfKind::Match => ' ',
        }
    }
}

/// One entry: a position in a file plus the text that produced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QfItem {
    pub path: PathBuf,
    /// 0-based line, as everywhere else in the engine — the `1` in
    /// `main.rs:1:5` becomes `0` here.
    pub line: usize,
    /// 0-based column.
    pub col: usize,
    pub text: String,
    pub kind: QfKind,
}

/// A list of entries plus the cursor `:cnext`/`:cprev` move.
#[derive(Debug, Clone, Default)]
pub struct QuickfixList {
    items: Vec<QfItem>,
    /// Index of the current entry. Meaningless when `items` is empty.
    current: usize,
    /// What produced the list, shown in the quickfix window's title
    /// (e.g. `:vimgrep /fn/`).
    title: String,
}

impl QuickfixList {
    pub fn new() -> Self {
        QuickfixList::default()
    }

    /// Replace the list's contents, resetting the cursor to the first entry —
    /// what every filler command (`:make`, `:grep`, `:vimgrep`) does.
    pub fn set(&mut self, items: Vec<QfItem>, title: impl Into<String>) {
        self.items = items;
        self.current = 0;
        self.title = title.into();
    }

    pub fn items(&self) -> &[QfItem] {
        &self.items
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Index of the current entry, or `None` when the list is empty.
    pub fn current_index(&self) -> Option<usize> {
        (!self.items.is_empty()).then_some(self.current)
    }

    pub fn current(&self) -> Option<&QfItem> {
        self.items.get(self.current)
    }

    /// `:cnext` / `:cprev`, by `count` entries.
    ///
    /// Vim errors at the end of the list rather than wrapping, so this saturates
    /// and returns the entry landed on (`None` only when the list is empty).
    pub fn advance(&mut self, count: isize) -> Option<&QfItem> {
        if self.items.is_empty() {
            return None;
        }
        let last = self.items.len() as isize - 1;
        let target = (self.current as isize + count).clamp(0, last);
        self.current = target as usize;
        self.items.get(self.current)
    }

    /// `:cfirst` / `:clast`.
    pub fn goto_end(&mut self, last: bool) -> Option<&QfItem> {
        if self.items.is_empty() {
            return None;
        }
        self.current = if last { self.items.len() - 1 } else { 0 };
        self.items.get(self.current)
    }

    /// `:cc [n]` — select entry `n` (0-based here, 1-based in the command).
    pub fn goto(&mut self, index: usize) -> Option<&QfItem> {
        if index >= self.items.len() {
            return None;
        }
        self.current = index;
        self.items.get(index)
    }
}

/// A compiled `:vimgrep` pattern.
///
/// The engine owns pattern *semantics* (Vim's magic flavor, via
/// [`crate::pattern`]) while the host owns the filesystem — so `:vimgrep` is
/// split: the host walks and reads files, this decides what matches. Wrapping
/// the regex also keeps the engine out of the frontend's dependency list.
pub struct Matcher {
    re: ctrlvim_regex::Regex,
}

impl Matcher {
    /// Compile a Vim pattern; `Err` carries a user-facing message.
    pub fn new(pattern: &str) -> Result<Matcher, String> {
        crate::pattern::compile(pattern)
            .map(|re| Matcher { re })
            .map_err(|e| format!("E486: invalid pattern: {e}"))
    }

    pub fn is_match(&self, line: &str) -> bool {
        self.re.is_match(line)
    }

    /// Character column of the first match on `line`, if any.
    pub fn first_match_col(&self, line: &str) -> Option<usize> {
        Some(self.re.find(line)?.start_char())
    }
}

/// Collect one entry per matching line of `text` — the per-file half of
/// `:vimgrep`, kept pure so the caller supplies the contents it read.
pub fn grep_text(matcher: &Matcher, path: &std::path::Path, text: &str) -> Vec<QfItem> {
    text.lines()
        .enumerate()
        .filter_map(|(i, line)| {
            let col = matcher.first_match_col(line)?;
            Some(QfItem {
                path: path.to_path_buf(),
                line: i,
                col,
                text: line.trim().to_string(),
                kind: QfKind::Match,
            })
        })
        .collect()
}

/// A stateful parser over a build's output lines.
///
/// Some compilers split one diagnostic across two lines — rustc emits
/// `error[E0308]: mismatched types` and *then* `  --> src/main.rs:2:18` — so a
/// line-at-a-time parse loses the message. This remembers the message and
/// attaches it to the location that follows, which is what makes the quickfix
/// pane readable rather than a list of bare positions.
#[derive(Default)]
pub struct OutputParser {
    /// A message awaiting its location line.
    pending: Option<(String, QfKind)>,
}

impl OutputParser {
    pub fn new() -> Self {
        OutputParser::default()
    }

    /// Feed one line, returning an entry if it completed one.
    pub fn push(&mut self, line: &str) -> Option<QfItem> {
        let trimmed = line.trim_start();

        // A location line completes whatever message came before it.
        if trimmed.starts_with("--> ") {
            let mut item = parse_line(line)?;
            if let Some((text, kind)) = self.pending.take() {
                item.text = text;
                item.kind = kind;
            }
            return Some(item);
        }

        // A bare severity line is the start of a two-line diagnostic. `error:`
        // and `error[E0308]:` both count; a line with a position in it does not
        // (that is the single-line gcc form, handled below).
        if let Some(kind) = leading_severity(trimmed) {
            if parse_line(line).is_none() {
                self.pending = Some((trimmed.to_string(), kind));
                return None;
            }
        }

        // Anything else: try the single-line forms. Reaching one means the
        // previous pending message never found a location, so drop it.
        let item = parse_line(line);
        if item.is_some() {
            self.pending = None;
        }
        item
    }
}

/// The severity a line leads with, if it leads with one.
fn leading_severity(line: &str) -> Option<QfKind> {
    let lower = line.to_ascii_lowercase();
    // `error[E0308]:` as well as plain `error:`.
    let head = lower.split([':', '[']).next()?;
    match head {
        "error" => Some(QfKind::Error),
        "warning" => Some(QfKind::Warning),
        "note" | "help" => Some(QfKind::Note),
        "info" => Some(QfKind::Info),
        _ => None,
    }
}

/// Parse one line of compiler/grep output into an entry.
///
/// This is *not* Vim's `'errorformat'` mini-language — it is a fixed set of the
/// formats ctrlvim's own toolchain emits. The signature is the seam an
/// errorformat engine would replace: line in, optional entry out.
///
/// Recognized:
/// - `path:line:col: error: msg` (gcc/clang, and cargo's rendered output)
/// - `path:line: msg` (grep -n, and older compilers)
/// - `  --> path:line:col` (rustc's location line)
pub fn parse_line(line: &str) -> Option<QfItem> {
    let trimmed = line.trim_start();
    if let Some(rest) = trimmed.strip_prefix("--> ") {
        // rustc location line: the message was on the previous line, which the
        // caller stitches on if it wants to.
        let (path, line_no, col) = split_position(rest.trim())?;
        return Some(QfItem {
            path,
            line: line_no,
            col,
            text: String::new(),
            kind: QfKind::Error,
        });
    }

    // `path:line:col: rest` or `path:line: rest`. Split off the message first so
    // a colon inside it can't confuse the position parse.
    let (position, text) = split_message(trimmed)?;
    let (path, line_no, col) = split_position(position)?;
    Some(QfItem { path, line: line_no, col, kind: kind_of(text), text: text.trim().to_string() })
}

/// Split `path:line[:col]: message` into its position and message halves.
/// Returns the whole thing as the position when there is no message.
fn split_message(line: &str) -> Option<(&str, &str)> {
    // Walk colons left to right; the position ends at the first colon whose
    // following segment isn't a number.
    let mut end = None;
    for (i, _) in line.match_indices(':') {
        let after = &line[i + 1..];
        let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
        if digits.is_empty() {
            end = Some(i);
            break;
        }
    }
    match end {
        Some(i) => Some((&line[..i], &line[i + 1..])),
        // No message part — the line is just a position.
        None => Some((line, "")),
    }
}

/// Split `path:line[:col]` into its pieces, converting to 0-based.
fn split_position(s: &str) -> Option<(PathBuf, usize, usize)> {
    let mut parts = s.rsplitn(3, ':');
    let last = parts.next()?;
    let middle = parts.next();
    let first = parts.next();
    match (first, middle) {
        // path:line:col
        (Some(path), Some(line)) => {
            let line = line.parse::<usize>().ok()?;
            let col = last.parse::<usize>().ok()?;
            Some((PathBuf::from(path), line.saturating_sub(1), col.saturating_sub(1)))
        }
        // path:line
        (None, Some(path)) => {
            let line = last.parse::<usize>().ok()?;
            Some((PathBuf::from(path), line.saturating_sub(1), 0))
        }
        _ => None,
    }
}

/// Classify an entry from its message text.
fn kind_of(text: &str) -> QfKind {
    let lower = text.trim_start().to_ascii_lowercase();
    if lower.starts_with("error") {
        QfKind::Error
    } else if lower.starts_with("warning") {
        QfKind::Warning
    } else if lower.starts_with("note") || lower.starts_with("help") {
        QfKind::Note
    } else if lower.starts_with("info") {
        QfKind::Info
    } else {
        QfKind::Match
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(line: usize) -> QfItem {
        QfItem {
            path: PathBuf::from("src/main.rs"),
            line,
            col: 0,
            text: format!("entry {line}"),
            kind: QfKind::Error,
        }
    }

    fn list_of(n: usize) -> QuickfixList {
        let mut qf = QuickfixList::new();
        qf.set((0..n).map(item).collect(), ":make");
        qf
    }

    #[test]
    fn navigation_saturates_instead_of_wrapping() {
        let mut qf = list_of(3);
        assert_eq!(qf.current_index(), Some(0));
        assert_eq!(qf.advance(1).unwrap().line, 1);
        assert_eq!(qf.advance(5).unwrap().line, 2, "past the end clamps to the last");
        assert_eq!(qf.advance(-9).unwrap().line, 0, "and before the start to the first");
        assert_eq!(qf.goto_end(true).unwrap().line, 2);
        assert_eq!(qf.goto(1).unwrap().line, 1);
        assert!(qf.goto(99).is_none(), "out of range selects nothing");
    }

    #[test]
    fn an_empty_list_navigates_to_nothing() {
        let mut qf = QuickfixList::new();
        assert!(qf.is_empty());
        assert_eq!(qf.current_index(), None);
        assert!(qf.advance(1).is_none());
        assert!(qf.goto_end(false).is_none());
        assert!(qf.current().is_none());
    }

    #[test]
    fn setting_the_list_resets_the_cursor() {
        let mut qf = list_of(3);
        qf.advance(2);
        qf.set(vec![item(7)], ":vimgrep /fn/");
        assert_eq!(qf.current_index(), Some(0));
        assert_eq!(qf.title(), ":vimgrep /fn/");
    }

    #[test]
    fn grep_text_finds_one_entry_per_matching_line() {
        let m = Matcher::new(r"\<fn\>").unwrap();
        let src = "// no match\nfn one() {}\nlet fnord = 1;\n    fn two() {}\n";
        let items = grep_text(&m, std::path::Path::new("a.rs"), src);
        assert_eq!(items.len(), 2, "`fnord` is not a word match");
        assert_eq!((items[0].line, items[0].col), (1, 0));
        assert_eq!((items[1].line, items[1].col), (3, 4), "column of the match");
        assert_eq!(items[1].text, "fn two() {}", "text is trimmed for display");
        assert_eq!(items[0].kind, QfKind::Match);
    }

    #[test]
    fn match_columns_are_characters_not_bytes() {
        let m = Matcher::new("x").unwrap();
        let items = grep_text(&m, std::path::Path::new("a.rs"), "// 🌍 x");
        assert_eq!(items[0].col, 5, "char column past the emoji");
    }

    #[test]
    fn an_invalid_pattern_is_an_error_not_a_panic() {
        assert!(Matcher::new(r"\(unclosed").is_err());
        // A bare `[` starts no valid collection, so Vim reads it as a literal
        // rather than rejecting the pattern.
        assert!(Matcher::new("[unclosed").is_ok());
    }

    #[test]
    fn parses_gcc_style_lines() {
        let qf = parse_line("src/main.c:12:5: error: expected ';'").unwrap();
        assert_eq!(qf.path, PathBuf::from("src/main.c"));
        assert_eq!((qf.line, qf.col), (11, 4), "1-based on screen, 0-based inside");
        assert_eq!(qf.kind, QfKind::Error);
        assert_eq!(qf.text, "error: expected ';'");
    }

    #[test]
    fn parses_grep_style_lines() {
        let qf = parse_line("src/app.rs:42:    fn main() {").unwrap();
        assert_eq!(qf.path, PathBuf::from("src/app.rs"));
        assert_eq!((qf.line, qf.col), (41, 0));
        assert_eq!(qf.kind, QfKind::Match);
        assert_eq!(qf.text, "fn main() {");
    }

    #[test]
    fn parses_rustc_location_lines() {
        let qf = parse_line("  --> crates/ctrlvim-editor/src/ex.rs:119:9").unwrap();
        assert_eq!(qf.path, PathBuf::from("crates/ctrlvim-editor/src/ex.rs"));
        assert_eq!((qf.line, qf.col), (118, 8));
    }

    #[test]
    fn output_parser_stitches_rustcs_two_line_diagnostics() {
        // Verbatim `cargo build` output shape.
        let lines = [
            "   Compiling brokenproj v0.1.0 (/tmp/brokenproj)",
            "error[E0308]: mismatched types",
            " --> src/main.rs:2:18",
            "  |",
            "2 |     let x: u32 = \"not a number\";",
            "  |            ---   ^^^^^^^^^^^^^^ expected `u32`, found `&str`",
            "",
            "error[E0425]: cannot find function `undefined_function` in this scope",
            " --> src/main.rs:3:5",
        ];
        let mut parser = OutputParser::new();
        let items: Vec<QfItem> = lines.iter().filter_map(|l| parser.push(l)).collect();

        assert_eq!(items.len(), 2, "one entry per diagnostic, not per line");
        assert_eq!(items[0].line, 1);
        assert_eq!(items[0].col, 17);
        assert_eq!(items[0].kind, QfKind::Error);
        assert_eq!(items[0].text, "error[E0308]: mismatched types", "message from the line above");
        assert_eq!(items[1].text, "error[E0425]: cannot find function `undefined_function` in this scope");
    }

    #[test]
    fn output_parser_still_handles_single_line_formats() {
        let mut parser = OutputParser::new();
        assert!(parser.push("Compiling foo").is_none());
        let item = parser.push("src/main.c:12:5: error: expected ';'").unwrap();
        assert_eq!(item.text, "error: expected ';'");
        assert_eq!(item.line, 11);
    }

    #[test]
    fn output_parser_drops_a_message_that_never_gets_a_location() {
        let mut parser = OutputParser::new();
        assert!(parser.push("error: could not compile `brokenproj`").is_none());
        // The next real entry must not inherit that dangling message.
        let item = parser.push("src/a.rs:1:1: warning: unused").unwrap();
        assert_eq!(item.text, "warning: unused");
        assert_eq!(item.kind, QfKind::Warning);
    }

    #[test]
    fn classifies_severity_from_the_message() {
        assert_eq!(parse_line("a.rs:1:1: warning: unused").unwrap().kind, QfKind::Warning);
        assert_eq!(parse_line("a.rs:1:1: note: defined here").unwrap().kind, QfKind::Note);
        assert_eq!(parse_line("a.rs:1:1: help: add a semicolon").unwrap().kind, QfKind::Note);
    }

    #[test]
    fn ignores_lines_that_are_not_positions() {
        assert!(parse_line("").is_none());
        assert!(parse_line("Compiling ctrlvim v0.1.0").is_none());
        assert!(parse_line("   |").is_none());
        assert!(parse_line("error: could not compile `ctrlvim`").is_none());
    }

    #[test]
    fn a_colon_in_the_message_does_not_confuse_the_position() {
        let qf = parse_line("src/x.rs:3:1: error: expected one of: `,` or `:`").unwrap();
        assert_eq!((qf.line, qf.col), (2, 0));
        assert_eq!(qf.text, "error: expected one of: `,` or `:`");
    }
}
