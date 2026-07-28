//! Cursor motions — the logical-movement layer distilled from `normal.c` and
//! `textobject.c`.
//!
//! Motions are pure functions over `(Buffer, cursor) -> new cursor` plus a
//! [`MotionKind`] describing how an operator should treat the swept range. We
//! deliberately keep this free of any window/fold/wrap display state (the C code
//! conflates them via `w_curswant` and fold lookups); screen-aware motions can
//! layer on top later.

use ctrlvim_text::Buffer;
use ctrlvim_types::Position;

/// Character class for word motions, mirroring `textobject.c`'s `cls()`:
/// whitespace, "keyword" chars, and other punctuation are three distinct
/// classes, and a word boundary is any transition between non-blank classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CharClass {
    Blank,
    Keyword,
    Punct,
}

fn class_of(c: char, big_word: bool) -> CharClass {
    if c.is_whitespace() {
        CharClass::Blank
    } else if big_word {
        // WORD motions (`W`/`B`/`E`): only blank vs non-blank matters.
        CharClass::Keyword
    } else if c.is_alphanumeric() || c == '_' {
        CharClass::Keyword
    } else {
        CharClass::Punct
    }
}

/// How a motion's swept text is treated by an operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MotionKind {
    /// Charwise, exclusive end (e.g. `w`, `h`, `l`).
    CharExclusive,
    /// Charwise, inclusive end (e.g. `e`, `$`, `f`).
    CharInclusive,
    /// Linewise (e.g. `j`, `k`, `G`, `gg`).
    Linewise,
}

/// The result of evaluating a motion.
#[derive(Debug, Clone, Copy)]
pub struct MotionResult {
    pub target: Position,
    pub kind: MotionKind,
}

fn line_chars(buf: &Buffer, line: usize) -> Vec<char> {
    buf.line(line).unwrap_or_default().chars().collect()
}

/// Clamp a cursor column to a valid position on its line. In Normal mode the
/// cursor rests *on* a character, so the max column is `len - 1` (or 0 for an
/// empty line); in operator/insert contexts one-past-end is allowed.
pub fn clamp_normal(buf: &Buffer, pos: Position) -> Position {
    let line = pos.line.min(buf.line_count().saturating_sub(1));
    let text = buf.line(line).unwrap_or_default();
    let n = text.chars().count();
    let max_col = n.saturating_sub(1);
    Position::new(line, pos.col.min(max_col))
}

/// `h` — left within the line.
pub fn left(_buf: &Buffer, pos: Position, count: usize) -> MotionResult {
    MotionResult {
        target: Position::new(pos.line, pos.col.saturating_sub(count.max(1))),
        kind: MotionKind::CharExclusive,
    }
}

/// `l` — right within the line (does not cross the line end in Normal mode).
pub fn right(buf: &Buffer, pos: Position, count: usize) -> MotionResult {
    let n = line_chars(buf, pos.line).len();
    let max_col = n.saturating_sub(1);
    MotionResult {
        target: Position::new(pos.line, (pos.col + count.max(1)).min(max_col)),
        kind: MotionKind::CharExclusive,
    }
}

/// `j` — down. Linewise; preserves column best-effort.
pub fn down(buf: &Buffer, pos: Position, count: usize) -> MotionResult {
    let line = (pos.line + count.max(1)).min(buf.line_count().saturating_sub(1));
    MotionResult {
        target: Position::new(line, pos.col),
        kind: MotionKind::Linewise,
    }
}

/// `k` — up. Linewise.
pub fn up(_buf: &Buffer, pos: Position, count: usize) -> MotionResult {
    let line = pos.line.saturating_sub(count.max(1));
    MotionResult {
        target: Position::new(line, pos.col),
        kind: MotionKind::Linewise,
    }
}

/// `j`/`k` with folds applied: each step moves one *visible* line, so a closed
/// fold is crossed in a single press rather than `count` presses through its
/// hidden body. `dir` is +1 for `j`, -1 for `k`.
///
/// With no closed folds this is exactly [`down`]/[`up`].
pub fn vertical_folded(
    buf: &Buffer,
    folds: &crate::fold::Folds,
    pos: Position,
    count: usize,
    dir: isize,
) -> MotionResult {
    let line_count = buf.line_count();
    let mut line = pos.line;
    for _ in 0..count.max(1) {
        match folds.next_visible(line, dir, line_count) {
            Some(next) => line = next,
            // At the edge of the buffer: stop, as `j` does on the last line.
            None => break,
        }
    }
    MotionResult {
        target: Position::new(line, pos.col),
        kind: MotionKind::Linewise,
    }
}

/// `}` (forward) / `{` (backward) — move a paragraph, distilled from
/// `textobject.c`'s `findpar()`.
///
/// A paragraph boundary is an **empty** line; a whitespace-only line is not one,
/// matching Vim. A run of consecutive empty lines counts as a single boundary,
/// so repeating `}` through a double-spaced file doesn't stop twice — that's
/// what the "skipped past content" state tracks.
///
/// Exclusive, so `d}` deletes up to but not including the boundary line. Running
/// off either end lands on the buffer's edge rather than refusing to move: at
/// the end that's the last line's *end*, so `d}` on the final paragraph deletes
/// through the last line.
pub fn paragraph(buf: &Buffer, pos: Position, count: usize, forward: bool) -> MotionResult {
    let n = buf.line_count();
    let mut line = pos.line;
    let mut at_edge = false;
    for _ in 0..count.max(1) {
        match next_paragraph(buf, line, forward, n) {
            Some(next) => line = next,
            None => {
                line = if forward { n.saturating_sub(1) } else { 0 };
                at_edge = true;
                break;
            }
        }
    }
    // Landing on the buffer's end means "everything to the end"; anywhere else
    // the boundary line itself is excluded, which column 0 expresses.
    let col = if at_edge && forward { line_chars(buf, line).len() } else { 0 };
    MotionResult {
        target: Position::new(line, col),
        kind: MotionKind::CharExclusive,
    }
}

/// One paragraph step, or `None` when the buffer edge comes first.
fn next_paragraph(buf: &Buffer, from: usize, forward: bool, n: usize) -> Option<usize> {
    let empty = |line: usize| buf.line(line).unwrap_or_default().is_empty();
    // Starting on content means the very next empty line is a boundary;
    // starting *on* a boundary means skipping its run first.
    let mut skipped_content = !empty(from);
    let mut line = from;
    loop {
        if forward {
            if line + 1 >= n {
                return None;
            }
            line += 1;
        } else {
            if line == 0 {
                return None;
            }
            line -= 1;
        }
        if empty(line) {
            if skipped_content {
                return Some(line);
            }
        } else {
            skipped_content = true;
        }
    }
}

/// `0` — first column.
pub fn line_start(_buf: &Buffer, pos: Position) -> MotionResult {
    MotionResult {
        target: Position::new(pos.line, 0),
        kind: MotionKind::CharExclusive,
    }
}

/// `^` — first non-blank column.
pub fn first_non_blank(buf: &Buffer, pos: Position) -> MotionResult {
    let chars = line_chars(buf, pos.line);
    let col = chars.iter().position(|c| !c.is_whitespace()).unwrap_or(0);
    MotionResult {
        target: Position::new(pos.line, col),
        kind: MotionKind::CharExclusive,
    }
}

/// `$` — end of line. Inclusive (so `d$` deletes through the last char).
pub fn line_end(buf: &Buffer, pos: Position) -> MotionResult {
    let n = line_chars(buf, pos.line).len();
    MotionResult {
        target: Position::new(pos.line, n.saturating_sub(1)),
        kind: MotionKind::CharInclusive,
    }
}

/// `gg` — first line (or `{count}gg`). Linewise.
pub fn goto_line_first(_buf: &Buffer, count: Option<usize>) -> MotionResult {
    let line = count.map(|c| c.saturating_sub(1)).unwrap_or(0);
    MotionResult {
        target: Position::new(line, 0),
        kind: MotionKind::Linewise,
    }
}

/// `G` — last line (or `{count}G`). Linewise.
pub fn goto_line_last(buf: &Buffer, count: Option<usize>) -> MotionResult {
    let last = buf.line_count().saturating_sub(1);
    let line = count.map(|c| c.saturating_sub(1).min(last)).unwrap_or(last);
    MotionResult {
        target: Position::new(line, 0),
        kind: MotionKind::Linewise,
    }
}

/// `w` — forward to the start of the next word. Exclusive.
pub fn word_forward(buf: &Buffer, pos: Position, count: usize, big: bool) -> MotionResult {
    let mut cur = pos;
    for _ in 0..count.max(1) {
        cur = word_forward_once(buf, cur, big);
    }
    MotionResult {
        target: cur,
        kind: MotionKind::CharExclusive,
    }
}

fn word_forward_once(buf: &Buffer, pos: Position, big: bool) -> Position {
    let total_lines = buf.line_count();
    let mut line = pos.line;
    let mut chars = line_chars(buf, line);
    let mut col = pos.col;

    // Skip the rest of the current word (same non-blank class).
    if col < chars.len() {
        let start_class = class_of(chars[col], big);
        if start_class != CharClass::Blank {
            while col < chars.len() && class_of(chars[col], big) == start_class {
                col += 1;
            }
        }
    }
    // Skip blanks, crossing line boundaries.
    loop {
        while col < chars.len() && class_of(chars[col], big) == CharClass::Blank {
            col += 1;
        }
        if col < chars.len() {
            break;
        }
        if line + 1 >= total_lines {
            // Clamp at end of buffer.
            return Position::new(line, chars.len().saturating_sub(1).max(0));
        }
        line += 1;
        chars = line_chars(buf, line);
        col = 0;
        // An empty line counts as a word start.
        if chars.is_empty() {
            return Position::new(line, 0);
        }
    }
    Position::new(line, col)
}

/// `b` — backward to the start of the current/previous word. Exclusive.
pub fn word_backward(buf: &Buffer, pos: Position, count: usize, big: bool) -> MotionResult {
    let mut cur = pos;
    for _ in 0..count.max(1) {
        cur = word_backward_once(buf, cur, big);
    }
    MotionResult {
        target: cur,
        kind: MotionKind::CharExclusive,
    }
}

fn word_backward_once(buf: &Buffer, pos: Position, big: bool) -> Position {
    let mut line = pos.line;
    let mut col = pos.col;
    let mut chars = line_chars(buf, line);

    // Step back one.
    if col == 0 {
        if line == 0 {
            return Position::new(0, 0);
        }
        line -= 1;
        chars = line_chars(buf, line);
        col = chars.len(); // will step into end below
    }
    if col > 0 {
        col -= 1;
    }
    // Skip blanks backward.
    loop {
        while col < chars.len() && class_of(chars[col], big) == CharClass::Blank {
            if col == 0 {
                break;
            }
            col -= 1;
        }
        if col < chars.len() && class_of(chars[col], big) != CharClass::Blank {
            break;
        }
        if line == 0 {
            return Position::new(0, 0);
        }
        line -= 1;
        chars = line_chars(buf, line);
        if chars.is_empty() {
            return Position::new(line, 0);
        }
        col = chars.len().saturating_sub(1);
    }
    // Move to the start of this word.
    let class = class_of(chars[col], big);
    while col > 0 && class_of(chars[col - 1], big) == class {
        col -= 1;
    }
    Position::new(line, col)
}

/// `e` — forward to the end of the current/next word. Inclusive.
pub fn word_end(buf: &Buffer, pos: Position, count: usize, big: bool) -> MotionResult {
    let mut cur = pos;
    for _ in 0..count.max(1) {
        cur = word_end_once(buf, cur, big);
    }
    MotionResult {
        target: cur,
        kind: MotionKind::CharInclusive,
    }
}

fn word_end_once(buf: &Buffer, pos: Position, big: bool) -> Position {
    let total_lines = buf.line_count();
    let mut line = pos.line;
    let mut chars = line_chars(buf, line);
    let mut col = pos.col + 1;

    // Skip blanks (and line boundaries).
    loop {
        while col < chars.len() && class_of(chars[col], big) == CharClass::Blank {
            col += 1;
        }
        if col < chars.len() {
            break;
        }
        if line + 1 >= total_lines {
            return Position::new(line, chars.len().saturating_sub(1));
        }
        line += 1;
        chars = line_chars(buf, line);
        col = 0;
    }
    // Advance to the end of this word.
    let class = class_of(chars[col], big);
    while col + 1 < chars.len() && class_of(chars[col + 1], big) == class {
        col += 1;
    }
    Position::new(line, col)
}

/// `f`/`F`/`t`/`T` — find `target` on the current line. `forward` picks the
/// direction; `till` (t/T) stops one column short of the target. Forward finds
/// are inclusive, backward finds are exclusive, which makes `df)`/`dF(` delete
/// the right span through [`crate::operator::OperatorSpan::from_motion`].
/// Returns `None` when the char (or enough of them) isn't found.
pub fn find_char(
    buf: &Buffer,
    pos: Position,
    target: char,
    count: usize,
    forward: bool,
    till: bool,
) -> Option<MotionResult> {
    let chars = line_chars(buf, pos.line);
    let n = chars.len() as isize;
    let mut col = pos.col as isize;
    let mut remaining = count.max(1);
    let target_col = loop {
        col += if forward { 1 } else { -1 };
        if col < 0 || col >= n {
            return None;
        }
        if chars[col as usize] == target {
            remaining -= 1;
            if remaining == 0 {
                break col as usize;
            }
        }
    };
    let final_col = if till {
        if forward { target_col.saturating_sub(1) } else { target_col + 1 }
    } else {
        target_col
    };
    let kind = if forward { MotionKind::CharInclusive } else { MotionKind::CharExclusive };
    Some(MotionResult { target: Position::new(pos.line, final_col), kind })
}

/// `%` — jump to the bracket matching the first `()[]{}` at or after the cursor
/// on the current line. Inclusive. `None` when there's no bracket / no match.
pub fn match_pair(buf: &Buffer, pos: Position) -> Option<MotionResult> {
    const PAIRS: [(char, char); 3] = [('(', ')'), ('[', ']'), ('{', '}')];
    let chars = line_chars(buf, pos.line);
    // Find the first bracket at or after the cursor on this line.
    let (bcol, &bracket) = chars.iter().enumerate().skip(pos.col).find(|(_, c)| {
        PAIRS.iter().any(|(o, cl)| **c == *o || **c == *cl)
    })?;
    let (open, close, forward) = PAIRS
        .iter()
        .find_map(|&(o, cl)| {
            if bracket == o {
                Some((o, cl, true))
            } else if bracket == cl {
                Some((o, cl, false))
            } else {
                None
            }
        })?;

    let total = buf.line_count();
    let mut depth = 0i32;
    let (mut line, mut col) = (pos.line, bcol);
    loop {
        let cur = line_chars(buf, line);
        if col < cur.len() {
            let c = cur[col];
            if c == open {
                depth += if forward { 1 } else { -1 };
            } else if c == close {
                depth += if forward { -1 } else { 1 };
            }
            if depth == 0 {
                return Some(MotionResult {
                    target: Position::new(line, col),
                    kind: MotionKind::CharInclusive,
                });
            }
        }
        if forward {
            col += 1;
            if col >= cur.len() {
                line += 1;
                col = 0;
                if line >= total {
                    return None;
                }
            }
        } else {
            if col == 0 {
                if line == 0 {
                    return None;
                }
                line -= 1;
                col = line_chars(buf, line).len().saturating_sub(1);
            } else {
                col -= 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buf(lines: &[&str]) -> Buffer {
        Buffer::from_lines(lines)
    }

    #[test]
    fn find_char_forward_and_till() {
        let b = buf(&["foo.bar.baz"]);
        // f. -> first dot at col 3
        assert_eq!(find_char(&b, Position::new(0, 0), '.', 1, true, false).unwrap().target.col, 3);
        // 2f. -> second dot at col 7
        assert_eq!(find_char(&b, Position::new(0, 0), '.', 2, true, false).unwrap().target.col, 7);
        // t. -> one before the first dot
        assert_eq!(find_char(&b, Position::new(0, 0), '.', 1, true, true).unwrap().target.col, 2);
        // missing char
        assert!(find_char(&b, Position::new(0, 0), 'z', 5, true, false).is_none());
    }

    #[test]
    fn find_char_backward() {
        let b = buf(&["foo.bar"]);
        // F. from end -> the dot, exclusive
        let r = find_char(&b, Position::new(0, 6), '.', 1, false, false).unwrap();
        assert_eq!(r.target.col, 3);
        assert_eq!(r.kind, MotionKind::CharExclusive);
    }

    #[test]
    fn match_pair_same_line_and_nested() {
        let b = buf(&["a(b(c)d)e"]);
        // From the first '(' (col 1) -> matching ')' at col 7.
        assert_eq!(match_pair(&b, Position::new(0, 1)).unwrap().target.col, 7);
        // From the inner ')' (col 5) backward -> inner '(' at col 3.
        assert_eq!(match_pair(&b, Position::new(0, 5)).unwrap().target.col, 3);
        // Cursor before a bracket skips forward to it.
        assert_eq!(match_pair(&b, Position::new(0, 0)).unwrap().target.col, 7);
    }

    #[test]
    fn match_pair_across_lines() {
        let b = buf(&["foo(", "  bar", ")"]);
        let r = match_pair(&b, Position::new(0, 3)).unwrap();
        assert_eq!((r.target.line, r.target.col), (2, 0));
    }

    #[test]
    fn hjkl_basic() {
        let b = buf(&["hello", "world"]);
        let p = Position::new(0, 2);
        assert_eq!(left(&b, p, 1).target, Position::new(0, 1));
        assert_eq!(right(&b, p, 1).target, Position::new(0, 3));
        assert_eq!(down(&b, p, 1).target.line, 1);
        assert_eq!(up(&b, Position::new(1, 2), 1).target.line, 0);
    }

    #[test]
    fn l_stops_at_last_char() {
        let b = buf(&["ab"]);
        assert_eq!(right(&b, Position::new(0, 1), 5).target, Position::new(0, 1));
    }

    #[test]
    fn dollar_and_zero_and_caret() {
        let b = buf(&["  foo bar"]);
        assert_eq!(line_end(&b, Position::new(0, 0)).target.col, 8);
        assert_eq!(line_start(&b, Position::new(0, 5)).target.col, 0);
        assert_eq!(first_non_blank(&b, Position::new(0, 0)).target.col, 2);
    }

    #[test]
    fn word_forward_within_line() {
        let b = buf(&["foo bar baz"]);
        let r = word_forward(&b, Position::new(0, 0), 1, false);
        assert_eq!(r.target, Position::new(0, 4)); // start of "bar"
        let r = word_forward(&b, Position::new(0, 0), 2, false);
        assert_eq!(r.target, Position::new(0, 8)); // start of "baz"
    }

    #[test]
    fn word_forward_punctuation_is_a_word() {
        let b = buf(&["foo.bar"]);
        let r = word_forward(&b, Position::new(0, 0), 1, false);
        assert_eq!(r.target, Position::new(0, 3)); // the "."
    }

    #[test]
    fn big_word_skips_punctuation() {
        let b = buf(&["foo.bar baz"]);
        let r = word_forward(&b, Position::new(0, 0), 1, true);
        assert_eq!(r.target, Position::new(0, 8)); // "baz", punctuation ignored
    }

    #[test]
    fn word_forward_crosses_lines() {
        let b = buf(&["foo", "bar"]);
        let r = word_forward(&b, Position::new(0, 0), 1, false);
        assert_eq!(r.target, Position::new(1, 0));
    }

    #[test]
    fn word_backward_motion() {
        let b = buf(&["foo bar baz"]);
        let r = word_backward(&b, Position::new(0, 8), 1, false);
        assert_eq!(r.target, Position::new(0, 4)); // "bar"
        let r = word_backward(&b, Position::new(0, 8), 2, false);
        assert_eq!(r.target, Position::new(0, 0)); // "foo"
    }

    #[test]
    fn word_end_motion() {
        let b = buf(&["foo bar"]);
        let r = word_end(&b, Position::new(0, 0), 1, false);
        assert_eq!(r.target, Position::new(0, 2)); // last char of "foo"
        assert_eq!(r.kind, MotionKind::CharInclusive);
    }

    #[test]
    fn gg_and_G() {
        let b = buf(&["a", "b", "c", "d"]);
        assert_eq!(goto_line_first(&b, None).target.line, 0);
        assert_eq!(goto_line_last(&b, None).target.line, 3);
        assert_eq!(goto_line_last(&b, Some(2)).target.line, 1);
    }
}
