//! Session — the top-level modal state machine wiring [`Editor`], [`Mode`], and
//! pending command state (count/register/operator) together.
//!
//! This is the Rust equivalent of the `normal_execute`/`insert_execute` dispatch
//! driven by `state.c`'s loop. The frontend feeds [`Key`]s in via [`Session::feed`]
//! and reads back cursor/mode/buffer state to render.

use crate::editor::Editor;
use crate::input::Key;
use crate::mode::{Mode, VisualKind};
use crate::motion::{self, MotionKind, MotionResult};
use crate::operator::{apply_operator, Operator, OperatorSpan};
use ctrlvim_text::{MotionType, YankReg};
use ctrlvim_types::Position;

/// Pending command-accumulation state (`cmdarg_T`/`oparg_T` in C).
#[derive(Default)]
struct Pending {
    count: Option<usize>,
    register: Option<char>,
    operator: Option<Operator>,
    /// True after `g` was pressed (awaiting the second char of `gg`/`g-`/`g+`).
    g_prefix: bool,
    /// True after `"` was pressed (awaiting a register name).
    await_register: bool,
    /// True after `<C-w>` (awaiting a window command).
    ctrl_w: bool,
}

impl Pending {
    fn clear(&mut self) {
        *self = Pending::default();
    }
    fn count_or(&self, default: usize) -> usize {
        self.count.unwrap_or(default)
    }
}

/// A modal editing session.
pub struct Session {
    pub editor: Editor,
    pub mode: Mode,
    pending: Pending,
    /// Text typed during the current Insert session, tracked so a single undo
    /// step covers the whole insertion (Neovim coalesces typing into one undo).
    insert_start: Option<Position>,
}

impl Session {
    pub fn new() -> Self {
        Session::from_editor(Editor::new())
    }

    /// Build a session around an existing editor (used when the Lua host and
    /// key-input dispatch must share one editor).
    pub fn from_editor(editor: Editor) -> Self {
        Session {
            editor,
            mode: Mode::Normal,
            pending: Pending::default(),
            insert_start: None,
        }
    }

    /// Convenience: build a session over the given lines.
    pub fn with_text(text: &str) -> Self {
        let mut s = Session::new();
        s.editor.load_str(text, None);
        s
    }

    /// Feed a whole `<...>`-encoded key sequence (for tests/scripts).
    pub fn feed_str(&mut self, s: &str) {
        for key in Key::parse_sequence(s) {
            self.feed(key);
        }
    }

    /// Process one keystroke.
    pub fn feed(&mut self, key: Key) {
        match &self.mode {
            Mode::Normal => self.feed_normal(key),
            Mode::Insert => self.feed_insert(key),
            Mode::Visual { .. } => self.feed_visual(key),
            Mode::Cmdline { .. } => self.feed_cmdline(key),
        }
    }

    // --- Normal mode ---

    fn feed_normal(&mut self, key: Key) {
        // Register selection: `"a`.
        if self.pending.await_register {
            self.pending.await_register = false;
            if let Key::Char(c) = key {
                self.pending.register = Some(c);
            }
            return;
        }

        // `g`-prefixed commands.
        if self.pending.g_prefix {
            self.pending.g_prefix = false;
            self.handle_g_command(key);
            return;
        }

        // `<C-w>`-prefixed window commands.
        if self.pending.ctrl_w {
            self.pending.ctrl_w = false;
            self.handle_window_command(key);
            return;
        }

        match key {
            Key::Char('"') => {
                self.pending.await_register = true;
            }
            // Count accumulation. A leading 0 is the `0` motion, not a count.
            Key::Char(c @ '1'..='9') => {
                let d = c.to_digit(10).unwrap() as usize;
                self.pending.count = Some(self.pending.count.unwrap_or(0) * 10 + d);
            }
            Key::Char('0') if self.pending.count.is_some() => {
                self.pending.count = Some(self.pending.count.unwrap() * 10);
            }
            Key::Char('g') => {
                self.pending.g_prefix = true;
            }
            // Operators.
            Key::Char('d') => self.begin_or_apply_operator(Operator::Delete, 'd'),
            Key::Char('y') => self.begin_or_apply_operator(Operator::Yank, 'y'),
            Key::Char('c') => self.begin_or_apply_operator(Operator::Change, 'c'),
            // `x`: delete char under cursor.
            Key::Char('x') => self.delete_char_under_cursor(),
            // Paste.
            Key::Char('p') => self.paste(true),
            Key::Char('P') => self.paste(false),
            // Enter insert mode.
            Key::Char('i') => self.enter_insert_at(self.editor.cursor()),
            Key::Char('a') => {
                let mut p = self.editor.cursor();
                p.col += 1;
                self.enter_insert_at(p);
            }
            Key::Char('I') => {
                let m = motion::first_non_blank(&self.editor.cur_buffer().text, self.editor.cursor());
                self.enter_insert_at(m.target);
            }
            Key::Char('A') => {
                let line = self.editor.cursor().line;
                let len = self.editor.cur_buffer().text.line_len(line);
                self.enter_insert_at(Position::new(line, len));
            }
            Key::Char('o') => self.open_line(true),
            Key::Char('O') => self.open_line(false),
            // Visual mode.
            Key::Char('v') => self.enter_visual(VisualKind::Char),
            Key::Char('V') => self.enter_visual(VisualKind::Line),
            Key::Ctrl('v') => self.enter_visual(VisualKind::Block),
            // Command line.
            Key::Char(':') => {
                self.mode = Mode::Cmdline { prefix: ':', buffer: String::new() };
            }
            // Undo / redo.
            Key::Char('u') => self.undo(),
            Key::Ctrl('r') => self.redo(),
            // Window commands.
            Key::Ctrl('w') => self.pending.ctrl_w = true,
            Key::Esc => self.pending.clear(),
            // Motions (may complete a pending operator).
            other => self.motion_key(other),
        }
    }

    /// `<C-w>{cmd}` window management: `s` split, `v` vsplit, `w` cycle,
    /// `q`/`c` close.
    fn handle_window_command(&mut self, key: Key) {
        match key {
            Key::Char('s') => {
                self.editor.split_current(false);
            }
            Key::Char('v') => {
                self.editor.split_current(true);
            }
            Key::Char('w') => self.editor.focus_next(),
            Key::Char('q') | Key::Char('c') => {
                let cur = self.editor.current_window_id();
                self.editor.close_window(cur);
            }
            _ => {}
        }
        self.pending.clear();
    }

    fn handle_g_command(&mut self, key: Key) {
        match key {
            Key::Char('g') => {
                let m = motion::goto_line_first(&self.editor.cur_buffer().text, self.pending.count);
                self.apply_motion_or_operator(m);
            }
            Key::Char('-') => self.undo_time(),
            Key::Char('+') => self.redo_time(),
            _ => self.pending.clear(),
        }
    }

    /// Resolve a motion key into a [`MotionResult`], if it is one.
    fn resolve_motion(&self, key: Key) -> Option<MotionResult> {
        let buf = &self.editor.cur_buffer().text;
        let cur = self.editor.cursor();
        let count = self.pending.count_or(1);
        Some(match key {
            Key::Char('h') | Key::Backspace => motion::left(buf, cur, count),
            Key::Char('l') | Key::Char(' ') => motion::right(buf, cur, count),
            Key::Char('j') => motion::down(buf, cur, count),
            Key::Char('k') => motion::up(buf, cur, count),
            Key::Char('0') => motion::line_start(buf, cur),
            Key::Char('^') => motion::first_non_blank(buf, cur),
            Key::Char('$') => motion::line_end(buf, cur),
            Key::Char('w') => motion::word_forward(buf, cur, count, false),
            Key::Char('W') => motion::word_forward(buf, cur, count, true),
            Key::Char('b') => motion::word_backward(buf, cur, count, false),
            Key::Char('B') => motion::word_backward(buf, cur, count, true),
            Key::Char('e') => motion::word_end(buf, cur, count, false),
            Key::Char('E') => motion::word_end(buf, cur, count, true),
            Key::Char('G') => motion::goto_line_last(buf, self.pending.count),
            _ => return None,
        })
    }

    fn motion_key(&mut self, key: Key) {
        if let Some(m) = self.resolve_motion(key) {
            self.apply_motion_or_operator(m);
        } else {
            // Unknown command: reset pending state.
            self.pending.clear();
        }
    }

    /// Either move the cursor (no operator pending) or apply the pending
    /// operator over the swept range.
    fn apply_motion_or_operator(&mut self, m: MotionResult) {
        if let Some(op) = self.pending.operator.take() {
            let cursor = self.editor.cursor();
            let span = OperatorSpan::from_motion(cursor, m.target, m.kind);
            let reg = self.pending.register;
            let outcome = apply_operator(&mut self.editor, op, span, reg);
            self.editor.set_cursor(outcome.cursor);
            if outcome.enter_insert {
                self.start_insert_session();
            }
            self.pending.clear();
        } else {
            // Pure movement. Linewise motions preserve the wanted column.
            let target = match m.kind {
                MotionKind::Linewise => Position::new(m.target.line, self.editor.cursor().col),
                _ => m.target,
            };
            self.editor.set_cursor(target);
            self.pending.clear();
        }
    }

    fn begin_or_apply_operator(&mut self, op: Operator, ch: char) {
        // Doubled operator (`dd`, `yy`, `cc`) → linewise over `count` lines.
        if self.pending.operator == Some(op) {
            let count = self.pending.count_or(1);
            let span = OperatorSpan::lines(self.editor.cursor(), count);
            let reg = self.pending.register;
            let outcome = apply_operator(&mut self.editor, op, span, reg);
            self.editor.set_cursor(outcome.cursor);
            if outcome.enter_insert {
                self.start_insert_session();
            }
            self.pending.clear();
            return;
        }
        let _ = ch;
        self.pending.operator = Some(op);
    }

    fn delete_char_under_cursor(&mut self) {
        let cur = self.editor.cursor();
        let line = self.editor.cur_buffer().text.line(cur.line).unwrap_or_default();
        if line.is_empty() {
            return;
        }
        let count = self.pending.count_or(1);
        // Delete `count` chars from the cursor (clamped to line end).
        let chars: Vec<(usize, char)> = line.char_indices().collect();
        let start_byte = cur.col.min(line.len());
        let end_idx = (chars.iter().position(|(b, _)| *b == start_byte).unwrap_or(chars.len())
            + count)
            .min(chars.len());
        let end_byte = chars.get(end_idx).map(|(b, _)| *b).unwrap_or(line.len());
        let deleted = line[start_byte..end_byte].to_string();
        self.editor.registers.delete(self.pending.register, YankReg::new(vec![deleted], MotionType::Char));
        self.editor.cur_buffer_mut().text.delete_range((cur.line, start_byte), (cur.line, end_byte));
        self.commit_undo(cur);
        self.editor.set_cursor(cur);
        self.pending.clear();
    }

    fn paste(&mut self, after: bool) {
        let reg = self.pending.register.unwrap_or('"');
        let yank = match self.editor.registers.read(reg) {
            Some(y) => y.clone(),
            None => {
                self.pending.clear();
                return;
            }
        };
        let cur = self.editor.cursor();
        match yank.motion {
            MotionType::Line => {
                let at = if after { cur.line + 1 } else { cur.line };
                self.editor.cur_buffer_mut().text.set_lines(at, at, &yank.lines);
                self.editor.cur_buffer_mut().marks.adjust_lines(at, at, yank.lines.len());
                self.commit_undo(Position::new(at, 0));
                self.editor.set_cursor(Position::new(at, 0));
            }
            _ => {
                let col = if after { (cur.col + 1).min(self.editor.cur_buffer().text.line_len(cur.line)) } else { cur.col };
                let text = yank.lines.join("\n");
                let (el, ec) = self.editor.cur_buffer_mut().text.insert(cur.line, col, &text);
                self.commit_undo(cur);
                self.editor.set_cursor(Position::new(el, ec.saturating_sub(1)));
            }
        }
        self.pending.clear();
    }

    fn open_line(&mut self, below: bool) {
        let cur = self.editor.cursor();
        let at = if below { cur.line + 1 } else { cur.line };
        self.editor.cur_buffer_mut().text.set_lines(at, at, &[""]);
        self.editor.cur_buffer_mut().marks.adjust_lines(at, at, 1);
        self.editor.cur_window_mut().cursor = Position::new(at, 0);
        self.start_insert_session();
        self.pending.clear();
    }

    fn enter_insert_at(&mut self, pos: Position) {
        let clamped = Position::new(
            pos.line.min(self.editor.cur_buffer().text.line_count().saturating_sub(1)),
            pos.col.min(self.editor.cur_buffer().text.line_len(pos.line)),
        );
        self.editor.cur_window_mut().cursor = clamped;
        self.start_insert_session();
        self.pending.clear();
    }

    fn start_insert_session(&mut self) {
        self.insert_start = Some(self.editor.cursor());
        self.mode = Mode::Insert;
    }

    // --- Insert mode ---

    fn feed_insert(&mut self, key: Key) {
        match key {
            Key::Esc => {
                // Commit one undo step for the whole insertion, move cursor left.
                let cur = self.editor.cursor();
                self.commit_undo(cur);
                let back = Position::new(cur.line, cur.col.saturating_sub(1));
                self.mode = Mode::Normal;
                self.insert_start = None;
                self.editor.set_cursor(back);
            }
            Key::Char(c) => {
                let cur = self.editor.cursor();
                let (l, col) = self.editor.cur_buffer_mut().text.insert(cur.line, cur.col, &c.to_string());
                self.editor.cur_window_mut().cursor = Position::new(l, col);
            }
            Key::Enter => {
                let cur = self.editor.cursor();
                let (l, col) = self.editor.cur_buffer_mut().text.insert(cur.line, cur.col, "\n");
                self.editor.cur_window_mut().cursor = Position::new(l, col);
            }
            Key::Tab => {
                let cur = self.editor.cursor();
                let (l, col) = self.editor.cur_buffer_mut().text.insert(cur.line, cur.col, "\t");
                self.editor.cur_window_mut().cursor = Position::new(l, col);
            }
            Key::Backspace => {
                let cur = self.editor.cursor();
                if cur.col > 0 {
                    let prev = prev_char_boundary(&self.editor.cur_buffer().text.line(cur.line).unwrap_or_default(), cur.col);
                    self.editor.cur_buffer_mut().text.delete_range((cur.line, prev), (cur.line, cur.col));
                    self.editor.cur_window_mut().cursor = Position::new(cur.line, prev);
                } else if cur.line > 0 {
                    // Join with previous line.
                    let prev = self.editor.cur_buffer().text.line(cur.line - 1).unwrap_or_default();
                    let prev_len = prev.len();
                    let this = self.editor.cur_buffer().text.line(cur.line).unwrap_or_default();
                    let joined = format!("{}{}", prev, this);
                    self.editor.cur_buffer_mut().text.set_lines(cur.line - 1, cur.line + 1, &[joined]);
                    self.editor.cur_window_mut().cursor = Position::new(cur.line - 1, prev_len);
                }
            }
            Key::Ctrl(_) => { /* completion etc. not yet implemented */ }
        }
    }

    // --- Visual mode ---

    fn enter_visual(&mut self, kind: VisualKind) {
        self.mode = Mode::Visual { anchor: self.editor.cursor(), kind };
        self.pending.clear();
    }

    fn feed_visual(&mut self, key: Key) {
        let (anchor, kind) = match &self.mode {
            Mode::Visual { anchor, kind } => (*anchor, *kind),
            _ => unreachable!(),
        };
        match key {
            Key::Esc => {
                self.mode = Mode::Normal;
            }
            Key::Char('d') | Key::Char('x') => self.visual_operator(Operator::Delete, anchor, kind),
            Key::Char('y') => self.visual_operator(Operator::Yank, anchor, kind),
            Key::Char('c') | Key::Char('s') => self.visual_operator(Operator::Change, anchor, kind),
            other => {
                // Movement extends the selection.
                if let Some(m) = self.resolve_motion(other) {
                    let target = match m.kind {
                        MotionKind::Linewise => Position::new(m.target.line, self.editor.cursor().col),
                        _ => m.target,
                    };
                    self.editor.set_cursor(target);
                }
                self.pending.clear();
            }
        }
    }

    fn visual_operator(&mut self, op: Operator, anchor: Position, kind: VisualKind) {
        let cursor = self.editor.cursor();
        let motion_kind = match kind {
            VisualKind::Line => MotionKind::Linewise,
            // Visual char selection is inclusive of the cursor character.
            _ => MotionKind::CharInclusive,
        };
        let span = OperatorSpan::from_motion(anchor, cursor, motion_kind);
        let reg = self.pending.register;
        let outcome = apply_operator(&mut self.editor, op, span, reg);
        self.editor.set_cursor(outcome.cursor);
        self.mode = Mode::Normal;
        if outcome.enter_insert {
            self.start_insert_session();
        }
        self.pending.clear();
    }

    // --- Cmdline mode ---

    fn feed_cmdline(&mut self, key: Key) {
        let (prefix, mut buffer) = match &self.mode {
            Mode::Cmdline { prefix, buffer } => (*prefix, buffer.clone()),
            _ => unreachable!(),
        };
        match key {
            Key::Esc => self.mode = Mode::Normal,
            Key::Enter => {
                self.mode = Mode::Normal;
                self.execute_ex(&buffer);
            }
            Key::Backspace => {
                buffer.pop();
                self.mode = Mode::Cmdline { prefix, buffer };
            }
            Key::Char(c) => {
                buffer.push(c);
                self.mode = Mode::Cmdline { prefix, buffer };
            }
            _ => {}
        }
    }

    /// A minimal Ex command executor. Full parsing is M6; this handles the few
    /// commands the demo needs (`:w` no-op ack, line numbers, `:%d`).
    fn execute_ex(&mut self, cmd: &str) {
        let cmd = cmd.trim();
        if let Ok(line) = cmd.parse::<usize>() {
            let target = line.saturating_sub(1).min(self.editor.cur_buffer().text.line_count() - 1);
            self.editor.set_cursor(Position::new(target, 0));
        } else if cmd == "$" {
            let last = self.editor.cur_buffer().text.line_count() - 1;
            self.editor.set_cursor(Position::new(last, 0));
        }
        // Other commands are no-ops for now.
    }

    // --- undo/redo plumbing ---

    fn commit_undo(&mut self, cursor: Position) {
        let text = self.editor.cur_buffer().text.clone();
        self.editor.cur_buffer_mut().changedtick += 1;
        self.editor.cur_buffer_mut().undo.commit(&text, cursor);
    }

    fn undo(&mut self) {
        if let Some((lines, cursor)) = self.editor.cur_buffer_mut().undo.undo() {
            self.restore(lines, cursor);
        }
        self.pending.clear();
    }

    fn redo(&mut self) {
        if let Some((lines, cursor)) = self.editor.cur_buffer_mut().undo.redo() {
            self.restore(lines, cursor);
        }
        self.pending.clear();
    }

    fn undo_time(&mut self) {
        if let Some((lines, cursor)) = self.editor.cur_buffer_mut().undo.undo_time() {
            self.restore(lines, cursor);
        }
        self.pending.clear();
    }

    fn redo_time(&mut self) {
        if let Some((lines, cursor)) = self.editor.cur_buffer_mut().undo.redo_time() {
            self.restore(lines, cursor);
        }
        self.pending.clear();
    }

    fn restore(&mut self, lines: Vec<String>, cursor: Position) {
        let n = lines.len();
        let buf = &mut self.editor.cur_buffer_mut().text;
        let old = buf.line_count();
        buf.set_lines(0, old, &lines);
        let _ = n;
        self.editor.set_cursor(cursor);
    }

    // --- introspection for the frontend ---

    pub fn mode_name(&self) -> &'static str {
        self.mode.short_name()
    }

    pub fn cursor(&self) -> Position {
        self.editor.cursor()
    }

    pub fn lines(&self) -> Vec<String> {
        self.editor.cur_buffer().text.lines()
    }
}

impl Default for Session {
    fn default() -> Self {
        Session::new()
    }
}

/// Find the byte index of the char boundary immediately before `col`.
fn prev_char_boundary(line: &str, col: usize) -> usize {
    line[..col.min(line.len())]
        .char_indices()
        .last()
        .map(|(b, _)| b)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hjkl_moves_cursor() {
        let mut s = Session::with_text("hello\nworld");
        s.feed_str("ll");
        assert_eq!(s.cursor(), Position::new(0, 2));
        s.feed_str("j");
        assert_eq!(s.cursor().line, 1);
        s.feed_str("h");
        assert_eq!(s.cursor(), Position::new(1, 1));
    }

    #[test]
    fn word_motion_with_count() {
        let mut s = Session::with_text("foo bar baz qux");
        s.feed_str("3w");
        assert_eq!(s.cursor(), Position::new(0, 12)); // "qux"
    }

    #[test]
    fn dw_deletes_word() {
        let mut s = Session::with_text("foo bar");
        s.feed_str("dw");
        assert_eq!(s.lines(), vec!["bar"]);
    }

    #[test]
    fn dd_deletes_line() {
        let mut s = Session::with_text("a\nb\nc");
        s.feed_str("jdd");
        assert_eq!(s.lines(), vec!["a", "c"]);
    }

    #[test]
    fn insert_text() {
        let mut s = Session::with_text("bar");
        s.feed_str("ifoo <Esc>");
        assert_eq!(s.lines(), vec!["foo bar"]);
        assert_eq!(s.mode_name(), "n");
    }

    #[test]
    fn append_and_open_line() {
        let mut s = Session::with_text("ab");
        s.feed_str("Ac<Esc>");
        assert_eq!(s.lines(), vec!["abc"]);
        s.feed_str("onew line<Esc>");
        assert_eq!(s.lines(), vec!["abc", "new line"]);
    }

    #[test]
    fn yank_and_paste_line() {
        let mut s = Session::with_text("one\ntwo");
        s.feed_str("yyp");
        assert_eq!(s.lines(), vec!["one", "one", "two"]);
    }

    #[test]
    fn x_deletes_char() {
        let mut s = Session::with_text("abc");
        s.feed_str("x");
        assert_eq!(s.lines(), vec!["bc"]);
    }

    #[test]
    fn undo_redo_roundtrip() {
        let mut s = Session::with_text("hello");
        s.feed_str("x");
        assert_eq!(s.lines(), vec!["ello"]);
        s.feed_str("u");
        assert_eq!(s.lines(), vec!["hello"]);
        s.feed_str("<C-r>");
        assert_eq!(s.lines(), vec!["ello"]);
    }

    #[test]
    fn visual_delete() {
        let mut s = Session::with_text("hello world");
        s.feed_str("vlld"); // select h,e,l -> delete 3 chars
        assert_eq!(s.lines(), vec!["lo world"]);
    }

    #[test]
    fn visual_line_delete() {
        let mut s = Session::with_text("a\nb\nc");
        s.feed_str("Vd");
        assert_eq!(s.lines(), vec!["b", "c"]);
    }

    #[test]
    fn cmdline_goto_line() {
        let mut s = Session::with_text("l1\nl2\nl3\nl4");
        s.feed_str(":3<CR>");
        assert_eq!(s.cursor().line, 2);
    }

    #[test]
    fn change_word_enters_insert() {
        let mut s = Session::with_text("foo bar");
        s.feed_str("cwbaz<Esc>");
        assert_eq!(s.lines(), vec!["bazbar"]);
    }

    #[test]
    fn gg_and_G_navigation() {
        let mut s = Session::with_text("1\n2\n3\n4\n5");
        s.feed_str("G");
        assert_eq!(s.cursor().line, 4);
        s.feed_str("gg");
        assert_eq!(s.cursor().line, 0);
    }

    #[test]
    fn count_dd_deletes_multiple_lines() {
        let mut s = Session::with_text("a\nb\nc\nd\ne");
        s.feed_str("2dd");
        assert_eq!(s.lines(), vec!["c", "d", "e"]);
    }

    #[test]
    fn ctrl_w_split_and_cycle_and_close() {
        let mut s = Session::with_text("shared buffer");
        assert_eq!(s.editor.window_count(), 1);
        s.feed_str("<C-w>v"); // vertical split
        assert_eq!(s.editor.window_count(), 2);
        s.feed_str("<C-w>s"); // horizontal split
        assert_eq!(s.editor.window_count(), 3);

        let before = s.editor.current_window_id();
        s.feed_str("<C-w>w"); // cycle focus
        assert_ne!(s.editor.current_window_id(), before);

        s.feed_str("<C-w>q"); // close current
        assert_eq!(s.editor.window_count(), 2);
    }

    #[test]
    fn cannot_close_last_window() {
        let mut s = Session::with_text("only");
        s.feed_str("<C-w>q");
        assert_eq!(s.editor.window_count(), 1);
    }

    #[test]
    fn layout_rects_cover_splits() {
        let mut s = Session::with_text("x");
        s.feed_str("<C-w>v");
        let rects = s.editor.layout_rects(80, 24);
        assert_eq!(rects.len(), 2);
        // Two side-by-side windows split the width.
        assert_eq!(rects[0].3 + rects[1].3, 80);
    }
}
