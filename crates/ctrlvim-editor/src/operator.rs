//! Operators (`d`/`y`/`c`) — the `ops.c` `do_pending_operator` core.
//!
//! Neovim deliberately routes *both* Normal-mode operator+motion (`dw`) and
//! Visual-mode operator application (`v...d`) through the same range
//! computation. We preserve that: [`OperatorSpan`] describes a range + how to
//! treat it, and [`apply_operator`] is the single point that yanks, mutates the
//! buffer, adjusts marks, and commits an undo step — no matter who produced the
//! span.

use crate::editor::Editor;
use crate::motion::MotionKind;
use ctrlvim_text::{Buffer, MotionType, YankReg};
use ctrlvim_types::Position;

/// The operators we implement. (Neovim has ~30; these are the core three plus
/// the ones the demo exercises.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operator {
    Delete,
    Yank,
    /// Change = delete then enter Insert mode (the caller handles the mode
    /// transition; this applies the delete part).
    Change,
    /// `>`/`<` — shift the spanned lines' indent right/left (linewise).
    Indent { right: bool },
    /// `gu`/`gU`/`g~` (and visual `u`/`U`/`~`) — case transforms.
    Lower,
    Upper,
    ToggleCase,
    /// `zf` — fold the swept lines. Unlike the others this changes no text; the
    /// session applies it, because folds are window state this layer can't see.
    Fold,
}

/// Number of spaces one `>`/`<` shift adds or removes.
const SHIFTWIDTH: usize = 4;

/// Column of a line's first non-blank character (or 0 for a blank line).
fn first_non_blank_col(buf: &Buffer, line: usize) -> usize {
    buf.line(line)
        .unwrap_or_default()
        .chars()
        .position(|c| !c.is_whitespace())
        .unwrap_or(0)
}

/// A resolved region to operate on, plus how to treat it.
#[derive(Debug, Clone, Copy)]
pub struct OperatorSpan {
    pub start: Position,
    pub end: Position,
    pub kind: MotionKind,
}

impl OperatorSpan {
    /// Build a span from the cursor + a motion result, ordering the endpoints
    /// and applying Vim's exclusive-motion adjustment.
    ///
    /// Per `:help exclusive`, an exclusive motion that ends in column 1 is
    /// rewritten, because operating "up to the start of a line" almost never
    /// means what it literally says:
    ///
    /// 1. the end moves back to the end of the previous line, and the motion
    ///    becomes inclusive — this is why `dw` on a line's last word does not
    ///    pull the following line up;
    /// 2. and if the start was at or before its line's first non-blank, the
    ///    whole thing becomes linewise — this is why `d}` from the start of a
    ///    paragraph deletes whole lines.
    pub fn from_motion(
        buf: &Buffer,
        cursor: Position,
        target: Position,
        kind: MotionKind,
    ) -> Self {
        let (start, end) = if cursor <= target {
            (cursor, target)
        } else {
            (target, cursor)
        };
        if kind == MotionKind::CharExclusive && end.col == 0 && end.line > start.line {
            let last = end.line - 1;
            if start.col <= first_non_blank_col(buf, start.line) {
                return OperatorSpan {
                    start: Position::new(start.line, 0),
                    end: Position::new(last, 0),
                    kind: MotionKind::Linewise,
                };
            }
            let len = buf.line(last).unwrap_or_default().chars().count();
            return OperatorSpan {
                start,
                end: Position::new(last, len.saturating_sub(1)),
                kind: MotionKind::CharInclusive,
            };
        }
        OperatorSpan { start, end, kind }
    }

    /// Build a linewise span covering `count` lines from `cursor` (for `dd`,
    /// `yy`).
    pub fn lines(cursor: Position, count: usize) -> Self {
        OperatorSpan {
            start: Position::new(cursor.line, 0),
            end: Position::new(cursor.line + count.saturating_sub(1), 0),
            kind: MotionKind::Linewise,
        }
    }
}

/// The result of applying an operator, so the caller can update cursor/mode.
pub struct OperatorOutcome {
    /// Where the cursor should land afterward.
    pub cursor: Position,
    /// True if the caller should now enter Insert mode (for `Change`).
    pub enter_insert: bool,
}

/// Apply `op` over `span` in the current buffer/window of `editor`, routing to
/// register `reg` (default unnamed). Handles yank, buffer mutation, and undo.
pub fn apply_operator(
    editor: &mut Editor,
    op: Operator,
    span: OperatorSpan,
    reg: Option<char>,
) -> OperatorOutcome {
    // Indent and case transforms don't yank/delete; handle them separately.
    match op {
        Operator::Indent { right } => return apply_indent(editor, span, right),
        Operator::Lower | Operator::Upper | Operator::ToggleCase => return apply_case(editor, op, span),
        // Handled by the session (it owns the window's folds); reaching here
        // means nothing to do to the text.
        Operator::Fold => {
            return OperatorOutcome { cursor: span.start, enter_insert: false }
        }
        _ => {}
    }
    match span.kind {
        MotionKind::Linewise => apply_linewise(editor, op, span, reg),
        MotionKind::CharExclusive => apply_charwise(editor, op, span, false, reg),
        MotionKind::CharInclusive => apply_charwise(editor, op, span, true, reg),
    }
}

/// `>`/`<` — add or remove one shiftwidth of leading spaces on each line.
fn apply_indent(editor: &mut Editor, span: OperatorSpan, right: bool) -> OperatorOutcome {
    let buf = &editor.cur_buffer().text;
    let last = buf.line_count().saturating_sub(1);
    let (l0, l1) = (span.start.line.min(last), span.end.line.min(last));
    let new: Vec<String> = (l0..=l1)
        .map(|i| {
            let line = buf.line(i).unwrap_or_default();
            if line.is_empty() {
                return line;
            }
            if right {
                format!("{}{}", " ".repeat(SHIFTWIDTH), line)
            } else {
                let strip = line.chars().take(SHIFTWIDTH).take_while(|c| *c == ' ').count();
                line[strip..].to_string()
            }
        })
        .collect();
    replace_lines(editor, l0, l1 + 1, &new);
    let cursor = crate::motion::first_non_blank(&editor.cur_buffer().text, Position::new(l0, 0)).target;
    commit_undo(editor, cursor);
    OperatorOutcome { cursor, enter_insert: false }
}

/// `gu`/`gU`/`g~` — transform the case of the spanned characters in place.
fn apply_case(editor: &mut Editor, op: Operator, span: OperatorSpan) -> OperatorOutcome {
    let transform = |c: char| match op {
        Operator::Lower => c.to_lowercase().next().unwrap_or(c),
        Operator::Upper => c.to_uppercase().next().unwrap_or(c),
        _ => {
            if c.is_uppercase() {
                c.to_lowercase().next().unwrap_or(c)
            } else {
                c.to_uppercase().next().unwrap_or(c)
            }
        }
    };
    let buf = &editor.cur_buffer().text;
    let last = buf.line_count().saturating_sub(1);
    match span.kind {
        MotionKind::Linewise => {
            let (l0, l1) = (span.start.line.min(last), span.end.line.min(last));
            let new: Vec<String> = (l0..=l1)
                .map(|i| buf.line(i).unwrap_or_default().chars().map(transform).collect())
                .collect();
            replace_lines(editor, l0, l1 + 1, &new);
            commit_undo(editor, Position::new(l0, 0));
            OperatorOutcome { cursor: Position::new(l0, 0), enter_insert: false }
        }
        kind => {
            // Charwise: single line only (multi-line case is rare; extend later).
            let inclusive = matches!(kind, MotionKind::CharInclusive);
            let line = buf.line(span.start.line).unwrap_or_default();
            let chars: Vec<char> = line.chars().collect();
            let s = span.start.col.min(chars.len());
            let mut e = span.end.col.min(chars.len());
            if inclusive {
                e = (e + 1).min(chars.len());
            }
            let new_line: String = chars
                .iter()
                .enumerate()
                .map(|(i, &c)| if i >= s && i < e { transform(c) } else { c })
                .collect();
            replace_lines(editor, span.start.line, span.start.line + 1, &[new_line]);
            commit_undo(editor, span.start);
            OperatorOutcome { cursor: span.start, enter_insert: false }
        }
    }
}

fn apply_linewise(
    editor: &mut Editor,
    op: Operator,
    span: OperatorSpan,
    reg: Option<char>,
) -> OperatorOutcome {
    let buf = &editor.cur_buffer().text;
    let last = buf.line_count().saturating_sub(1);
    let l0 = span.start.line.min(last);
    let l1 = span.end.line.min(last);
    let lines: Vec<String> = (l0..=l1).map(|i| buf.line(i).unwrap_or_default()).collect();

    // Yank the affected lines.
    let yreg = YankReg::new(lines.clone(), MotionType::Line);
    route_to_register(editor, op, reg, yreg);

    if op == Operator::Yank {
        return OperatorOutcome {
            cursor: Position::new(l0, 0),
            enter_insert: false,
        };
    }

    // Delete (or change) the lines.
    if op == Operator::Change {
        // Change on linewise leaves one empty line to type into.
        replace_lines(editor, l0, l1 + 1, &[""]);
        commit_undo(editor, Position::new(l0, 0));
        OperatorOutcome {
            cursor: Position::new(l0, 0),
            enter_insert: true,
        }
    } else {
        replace_lines(editor, l0, l1 + 1, &[] as &[&str]);
        let new_last = editor.cur_buffer().text.line_count().saturating_sub(1);
        let cursor_line = l0.min(new_last);
        commit_undo(editor, Position::new(cursor_line, 0));
        OperatorOutcome {
            cursor: Position::new(cursor_line, 0),
            enter_insert: false,
        }
    }
}

fn apply_charwise(
    editor: &mut Editor,
    op: Operator,
    span: OperatorSpan,
    inclusive: bool,
    reg: Option<char>,
) -> OperatorOutcome {
    let buf = &editor.cur_buffer().text;
    let mut end = span.end;
    if inclusive {
        // Extend one char past the last char (byte-wise) for inclusive motions.
        let line = buf.line(end.line).unwrap_or_default();
        let char_end: Vec<(usize, char)> = line.char_indices().collect();
        // Advance end.col to the byte after the character at end.col.
        if let Some(&(byte, ch)) = char_end.iter().find(|(b, _)| *b == end.col) {
            end.col = byte + ch.len_utf8();
        } else {
            end.col = line.len();
        }
    }

    // Extract the text (single-line case for now; multi-line charwise joins).
    let text = extract_charwise(buf, span.start, end);
    let yreg = YankReg::new(text, MotionType::Char);
    route_to_register(editor, op, reg, yreg);

    if op == Operator::Yank {
        return OperatorOutcome {
            cursor: span.start,
            enter_insert: false,
        };
    }

    editor
        .cur_buffer_mut()
        .text
        .delete_range((span.start.line, span.start.col), (end.line, end.col));
    let enter_insert = op == Operator::Change;
    commit_undo(editor, span.start);
    OperatorOutcome {
        cursor: span.start,
        enter_insert,
    }
}

fn extract_charwise(buf: &ctrlvim_text::Buffer, start: Position, end: Position) -> Vec<String> {
    if start.line == end.line {
        let line = buf.line(start.line).unwrap_or_default();
        let s = start.col.min(line.len());
        let e = end.col.min(line.len());
        vec![line[s..e].to_string()]
    } else {
        let mut out = Vec::new();
        let first = buf.line(start.line).unwrap_or_default();
        out.push(first[start.col.min(first.len())..].to_string());
        for l in (start.line + 1)..end.line {
            out.push(buf.line(l).unwrap_or_default());
        }
        let last = buf.line(end.line).unwrap_or_default();
        out.push(last[..end.col.min(last.len())].to_string());
        out
    }
}

fn route_to_register(editor: &mut Editor, op: Operator, reg: Option<char>, yreg: YankReg) {
    match op {
        Operator::Yank => editor.registers.yank(reg, yreg),
        // Only Delete/Change reach here; the transform operators are handled
        // before any register routing.
        _ => editor.registers.delete(reg, yreg),
    }
}

fn replace_lines<S: AsRef<str>>(editor: &mut Editor, start: usize, end: usize, repl: &[S]) {
    let old_count = end - start;
    editor.cur_buffer_mut().text.set_lines(start, end, repl);
    editor
        .cur_buffer_mut()
        .marks
        .adjust_lines(start, end, repl.len());
    let _ = old_count;
}

fn commit_undo(editor: &mut Editor, cursor: Position) {
    let text = editor.cur_buffer().text.clone();
    editor.cur_buffer_mut().changedtick += 1;
    editor.cur_buffer_mut().undo.commit(&text, cursor);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::motion;

    fn ed(lines: &[&str]) -> Editor {
        let mut e = Editor::new();
        e.load_str(&lines.join("\n"), None);
        e
    }

    #[test]
    fn delete_word_charwise() {
        let mut e = ed(&["foo bar"]);
        let m = motion::word_forward(&e.cur_buffer().text, Position::new(0, 0), 1, false);
        let span = OperatorSpan::from_motion(&e.cur_buffer().text, Position::new(0, 0), m.target, m.kind);
        apply_operator(&mut e, Operator::Delete, span, None);
        assert_eq!(e.cur_buffer().text.line(0).as_deref(), Some("bar"));
        assert_eq!(e.registers.read('"').unwrap().lines, vec!["foo "]);
    }

    #[test]
    fn delete_line_linewise() {
        let mut e = ed(&["a", "b", "c"]);
        e.set_cursor(Position::new(1, 0));
        let span = OperatorSpan::lines(e.cursor(), 1);
        apply_operator(&mut e, Operator::Delete, span, None);
        assert_eq!(e.cur_buffer().text.lines(), vec!["a", "c"]);
        assert_eq!(e.registers.read('"').unwrap().lines, vec!["b"]);
    }

    #[test]
    fn yank_does_not_modify_buffer() {
        let mut e = ed(&["hello"]);
        let m = motion::line_end(&e.cur_buffer().text, Position::new(0, 0));
        let span = OperatorSpan::from_motion(&e.cur_buffer().text, Position::new(0, 0), m.target, m.kind);
        apply_operator(&mut e, Operator::Yank, span, None);
        assert_eq!(e.cur_buffer().text.line(0).as_deref(), Some("hello"));
        assert_eq!(e.registers.read('"').unwrap().lines, vec!["hello"]);
    }

    #[test]
    fn dollar_delete_is_inclusive() {
        let mut e = ed(&["hello"]);
        e.set_cursor(Position::new(0, 2));
        let m = motion::line_end(&e.cur_buffer().text, e.cursor());
        let span = OperatorSpan::from_motion(&e.cur_buffer().text, e.cursor(), m.target, m.kind);
        apply_operator(&mut e, Operator::Delete, span, None);
        assert_eq!(e.cur_buffer().text.line(0).as_deref(), Some("he"));
    }
}
