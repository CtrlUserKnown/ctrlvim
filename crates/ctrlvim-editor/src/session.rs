//! Session — the top-level modal state machine wiring [`Editor`], [`Mode`], and
//! pending command state (count/register/operator) together.
//!
//! This is the Rust equivalent of the `normal_execute`/`insert_execute` dispatch
//! driven by `state.c`'s loop. The frontend feeds [`Key`]s in via [`Session::feed`]
//! and reads back cursor/mode/buffer state to render.

use crate::editor::Editor;
use crate::ex::{parse_ex, ExEffect, ExParsed, QuickfixCmd, TagCmd};
use crate::input::Key;
use crate::keymap::{Keymap, KeymapMatch};
use crate::mode::{Mode, Selection, VisualKind};
use crate::motion::{self, MotionKind, MotionResult};
use crate::operator::{apply_operator, Operator, OperatorSpan};
use crate::range::{self, Addr, Address, RangeSpec};
use crate::textobject;
use ctrlvim_text::{MotionType, YankReg};
use ctrlvim_types::Position;
use crate::pattern::{compile as compile_pattern, compile_opts as compile_pattern_opts};
// `:s` and the project-wide replace panel must translate replacements the same
// way, so both use the one in `replace`.
use crate::replace::vim_replacement;

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
    /// True after `z` (awaiting a fold/scroll command).
    z_prefix: bool,
    /// After `f`/`F`/`t`/`T`: `(forward, till)`, awaiting the target char.
    await_find: Option<(bool, bool)>,
    /// True after `r`, awaiting the replacement char.
    await_replace: bool,
    /// After an operator + `i`/`a`: `around`, awaiting the object char.
    await_textobject: Option<bool>,
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
    /// Normal-mode key mappings (`<leader>` chords etc.), consulted by [`feed`].
    pub keymap: Keymap,
    pending: Pending,
    /// Keys buffered while a mapping's left-hand side is still being matched
    /// (the start of the M3 typeahead buffer).
    map_pending: Vec<Key>,
    /// Nonzero while replaying a mapping's right-hand side, so the expansion is
    /// non-recursive (noremap) and can't loop.
    no_remap: u32,
    /// Host effects requested by Ex commands, drained via [`take_effects`].
    effects: Vec<ExEffect>,
    /// Text typed during the current Insert session, tracked so a single undo
    /// step covers the whole insertion (Neovim coalesces typing into one undo).
    insert_start: Option<Position>,
    /// Last `f`/`F`/`t`/`T`: `(target, forward, till)`, for `;`/`,`.
    last_find: Option<(char, bool, bool)>,
    /// Last `/`/`?` search: `(pattern, forward)`, for `n`/`N` and `:s//`.
    last_search: Option<(String, bool)>,
    /// Last `:s` `(pattern, replacement, flags)`, for a bare `:s` repeat.
    last_subst: Option<(String, String, String)>,
    /// User commands (`:command Name expansion`), keyed by name.
    user_commands: std::collections::HashMap<String, String>,
    /// Whether search matches are currently highlighted (cleared by `:noh`).
    pub search_highlight: bool,
    /// The last Visual selection `(start, end)`, resolving `'<` / `'>`.
    last_visual: Option<(Position, Position)>,
    /// Dot-repeat: keys of the current normal-mode command, the last completed
    /// change to replay on `.`, and a guard while replaying it.
    recording: Vec<Key>,
    last_change: Vec<Key>,
    dot_replaying: bool,
    cmd_first: Option<Key>,
    change_tick_at_start: u64,
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
            keymap: default_keymap(),
            pending: Pending::default(),
            map_pending: Vec::new(),
            no_remap: 0,
            effects: Vec::new(),
            insert_start: None,
            last_find: None,
            last_search: None,
            last_subst: None,
            user_commands: std::collections::HashMap::new(),
            search_highlight: false,
            last_visual: None,
            recording: Vec::new(),
            last_change: Vec::new(),
            dot_replaying: false,
            cmd_first: None,
            change_tick_at_start: 0,
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

    /// Process one keystroke, recording it for dot-repeat.
    pub fn feed(&mut self, key: Key) {
        // Only user-level keys (not mapping expansions or `.` replays) are
        // recorded, and a fresh recording starts at each resting command boundary.
        let record = self.no_remap == 0 && !self.dot_replaying;
        if record && matches!(self.mode, Mode::Normal) && self.at_rest() {
            self.recording.clear();
            self.cmd_first = Some(key);
            self.change_tick_at_start = self.editor.cur_buffer().changedtick;
        }
        if record {
            self.recording.push(key);
        }

        let ticks_before = self.editor.cur_buffer().changedtick;
        self.route(key);
        // Computed folds (`foldmethod=indent`) follow the text, so re-derive
        // them whenever it changed. No-op under the default `manual`.
        if self.editor.cur_buffer().changedtick != ticks_before {
            self.refresh_folds();
        }

        // Command finished at a resting boundary and changed the buffer → it's
        // the new "last change" (but a `.` command must not overwrite itself).
        if record
            && matches!(self.mode, Mode::Normal)
            && self.at_rest()
            && self.cmd_first != Some(Key::Char('.'))
            && self.editor.cur_buffer().changedtick != self.change_tick_at_start
        {
            self.last_change = self.recording.clone();
        }
    }

    /// Dispatch one keystroke to the active mode.
    fn route(&mut self, key: Key) {
        match &self.mode {
            Mode::Normal => self.feed_normal(key),
            Mode::Insert => self.feed_insert(key),
            Mode::Visual { .. } => self.feed_visual(key),
            Mode::Cmdline { .. } => self.feed_cmdline(key),
        }
    }

    /// No pending command state — a clean command boundary.
    fn at_rest(&self) -> bool {
        let p = &self.pending;
        p.count.is_none()
            && p.operator.is_none()
            && p.register.is_none()
            && !p.g_prefix
            && !p.ctrl_w
            && !p.z_prefix
            && !p.await_register
            && p.await_find.is_none()
            && !p.await_replace
            && p.await_textobject.is_none()
            && self.map_pending.is_empty()
    }

    // --- Normal mode ---

    fn feed_normal(&mut self, key: Key) {
        // Awaited operands (`f{c}`, `r{c}`, `di{obj}`) consume the next key
        // literally, before mappings or the command table.
        if let Some((forward, till)) = self.pending.await_find.take() {
            self.do_find(key, forward, till);
            return;
        }
        if self.pending.await_replace {
            self.pending.await_replace = false;
            self.do_replace(key);
            return;
        }
        if let Some(around) = self.pending.await_textobject.take() {
            self.apply_textobject(around, key);
            return;
        }

        // Keymap/typeahead: buffer keys that could form a `<leader>`-style
        // mapping and expand it, unless we're replaying an expansion (noremap).
        if self.no_remap == 0 && self.consult_keymap(key) {
            return;
        }

        // Doubled case operator (`guu`/`gUU`/`g~~`) → linewise over `count`.
        if let Some(op) = self.pending.operator {
            let doubled = matches!(
                (op, key),
                (Operator::Lower, Key::Char('u'))
                    | (Operator::Upper, Key::Char('U'))
                    | (Operator::ToggleCase, Key::Char('~'))
            );
            if doubled {
                let count = self.pending.count_or(1);
                let span = OperatorSpan::lines(self.editor.cursor(), count);
                let outcome = apply_operator(&mut self.editor, op, span, None);
                self.editor.set_cursor(outcome.cursor);
                self.pending.clear();
                return;
            }
            // With an operator pending, `i`/`a` begin a text object (`diw`).
            match key {
                Key::Char('i') => {
                    self.pending.await_textobject = Some(false);
                    return;
                }
                Key::Char('a') => {
                    self.pending.await_textobject = Some(true);
                    return;
                }
                _ => {}
            }
        }

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

        // `z`-prefixed fold commands (`zf`, `za`, `zR`, …).
        if self.pending.z_prefix {
            self.pending.z_prefix = false;
            if !self.fold_key(key) {
                self.pending.clear();
            }
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
            Key::Char('>') => self.begin_or_apply_operator(Operator::Indent { right: true }, '>'),
            Key::Char('<') => self.begin_or_apply_operator(Operator::Indent { right: false }, '<'),
            // Char-search motions (await their target char).
            Key::Char('f') => self.pending.await_find = Some((true, false)),
            Key::Char('F') => self.pending.await_find = Some((false, false)),
            Key::Char('t') => self.pending.await_find = Some((true, true)),
            Key::Char('T') => self.pending.await_find = Some((false, true)),
            Key::Char(';') => self.repeat_find(false),
            Key::Char(',') => self.repeat_find(true),
            // `%` match pair.
            Key::Char('%') => {
                if let Some(m) = motion::match_pair(&self.editor.cur_buffer().text, self.editor.cursor()) {
                    self.apply_motion_or_operator(m);
                } else {
                    self.pending.clear();
                }
            }
            // `r` replace char (awaits the replacement); `~` toggle case + advance.
            Key::Char('r') => self.pending.await_replace = true,
            Key::Char('~') => self.toggle_case_char(),
            // `.` repeat the last change.
            Key::Char('.') => self.dot_repeat(),
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
            // Search command line.
            Key::Char('/') => {
                self.mode = Mode::Cmdline { prefix: '/', buffer: String::new() };
            }
            Key::Char('?') => {
                self.mode = Mode::Cmdline { prefix: '?', buffer: String::new() };
            }
            // Repeat the last search (`n` same direction, `N` reversed).
            Key::Char('n') => self.search_next(true),
            Key::Char('N') => self.search_next(false),
            // Undo / redo.
            Key::Char('u') => self.undo(),
            Key::Ctrl('r') => self.redo(),
            // Window commands.
            Key::Ctrl('w') => self.pending.ctrl_w = true,
            // Fold commands (`zf`, `za`, `zo`, `zR`, …).
            Key::Char('z') => self.pending.z_prefix = true,
            // Tags: `<C-]>` jumps to the definition under the cursor, `<C-t>`
            // returns.
            Key::Ctrl(']') => {
                self.tag_lookup(None);
                self.pending.clear();
            }
            Key::Ctrl('t') => {
                self.tag_pop();
                self.pending.clear();
            }
            Key::Esc => self.pending.clear(),
            // Motions (may complete a pending operator).
            other => self.motion_key(other),
        }
    }

    /// True when it's safe to begin matching a mapping — i.e. no count /
    /// operator / register / prefix command is mid-flight.
    fn map_ready(&self) -> bool {
        self.pending.count.is_none()
            && self.pending.operator.is_none()
            && self.pending.register.is_none()
            && !self.pending.g_prefix
            && !self.pending.ctrl_w
            && !self.pending.await_register
            && self.pending.await_find.is_none()
            && !self.pending.await_replace
            && self.pending.await_textobject.is_none()
    }

    /// Feed `key` into the mapping matcher. Returns `true` when the key was
    /// consumed by the mapping layer (buffered, expanded, or replayed here).
    fn consult_keymap(&mut self, key: Key) -> bool {
        // Only start buffering from a clean state, and only if `key` could
        // actually begin a mapping.
        if self.map_pending.is_empty() && (!self.map_ready() || !self.keymap.can_start_normal(key)) {
            return false;
        }
        self.map_pending.push(key);
        match self.keymap.match_normal(&self.map_pending) {
            KeymapMatch::Full(rhs) => {
                self.map_pending.clear();
                self.replay(rhs);
            }
            KeymapMatch::Prefix => {} // wait for the next key
            KeymapMatch::None => {
                // The buffered keys don't form a mapping; use them literally.
                let buffered = std::mem::take(&mut self.map_pending);
                self.replay(buffered);
            }
        }
        true
    }

    /// Re-feed a key sequence non-recursively (mapping right-hand sides and
    /// broken prefixes are noremap, which also prevents infinite loops).
    fn replay(&mut self, keys: Vec<Key>) {
        self.no_remap += 1;
        for k in keys {
            self.feed(k);
        }
        self.no_remap -= 1;
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
            // `gt`/`gT` switch tabs (`{count}gt` jumps to tab N). Emitted as a
            // host effect since the tab list lives in the frontend.
            Key::Char('t') => {
                let effect = match self.pending.count {
                    Some(n) => ExEffect::Buffer(crate::ex::BufferCmd::Goto(n)),
                    None => ExEffect::Buffer(crate::ex::BufferCmd::Next),
                };
                self.queue_effect(effect);
                self.pending.clear();
            }
            Key::Char('T') => {
                self.queue_effect(ExEffect::Buffer(crate::ex::BufferCmd::Prev));
                self.pending.clear();
            }
            // Case operators await a motion (`guw`) or double (`guu`).
            Key::Char('u') => self.pending.operator = Some(Operator::Lower),
            Key::Char('U') => self.pending.operator = Some(Operator::Upper),
            Key::Char('~') => self.pending.operator = Some(Operator::ToggleCase),
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
            // Vertical motion is fold-aware: a closed fold is one step, not one
            // step per line inside it.
            Key::Char('j') => motion::vertical_folded(buf, self.folds(), cur, count, 1),
            Key::Char('k') => motion::vertical_folded(buf, self.folds(), cur, count, -1),
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
            // Paragraph motions (`Shift+]` / `Shift+[`).
            Key::Char('}') => motion::paragraph(buf, cur, count, true),
            Key::Char('{') => motion::paragraph(buf, cur, count, false),
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
            // `zf{motion}` folds the swept lines instead of editing them.
            if op == Operator::Fold {
                let (start, end) = if cursor.line <= m.target.line {
                    (cursor.line, m.target.line)
                } else {
                    (m.target.line, cursor.line)
                };
                self.create_fold(start, end);
                self.pending.clear();
                return;
            }
            let span = OperatorSpan::from_motion(&self.editor.cur_buffer().text, cursor, m.target, m.kind);
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

    /// `f{c}`/`t{c}` (and backward variants): resolve the target char into a
    /// motion, remembering it for `;`/`,`.
    fn do_find(&mut self, key: Key, forward: bool, till: bool) {
        let Key::Char(target) = key else {
            self.pending.clear();
            return;
        };
        self.last_find = Some((target, forward, till));
        let count = self.pending.count_or(1);
        let cur = self.editor.cursor();
        match motion::find_char(&self.editor.cur_buffer().text, cur, target, count, forward, till) {
            Some(m) => self.apply_motion_or_operator(m),
            None => self.pending.clear(),
        }
    }

    /// `;`/`,`: repeat the last `f`/`t` in the same (`;`) or opposite (`,`) direction.
    fn repeat_find(&mut self, opposite: bool) {
        let Some((target, forward, till)) = self.last_find else {
            self.pending.clear();
            return;
        };
        let dir = forward ^ opposite;
        let count = self.pending.count_or(1);
        let cur = self.editor.cursor();
        match motion::find_char(&self.editor.cur_buffer().text, cur, target, count, dir, till) {
            Some(m) => self.apply_motion_or_operator(m),
            None => self.pending.clear(),
        }
    }

    /// `r{c}`: replace `count` chars under the cursor with `c` (no-op if it would
    /// run past the line end, matching Vim).
    fn do_replace(&mut self, key: Key) {
        let Key::Char(ch) = key else {
            self.pending.clear();
            return;
        };
        let count = self.pending.count_or(1);
        let cur = self.editor.cursor();
        let chars: Vec<char> = self.editor.cur_buffer().text.line(cur.line).unwrap_or_default().chars().collect();
        if cur.col + count > chars.len() {
            self.pending.clear();
            return;
        }
        let end = cur.col + count;
        let mut new: String = chars[..cur.col].iter().collect();
        new.extend(std::iter::repeat(ch).take(count));
        new.extend(chars[end..].iter());
        self.editor.cur_buffer_mut().text.set_lines(cur.line, cur.line + 1, &[new]);
        self.commit_undo(cur);
        self.editor.set_cursor(Position::new(cur.line, end.saturating_sub(1)));
        self.pending.clear();
    }

    /// `~`: toggle the case of `count` chars, advancing the cursor past them.
    fn toggle_case_char(&mut self) {
        let count = self.pending.count_or(1);
        let cur = self.editor.cursor();
        let chars: Vec<char> = self.editor.cur_buffer().text.line(cur.line).unwrap_or_default().chars().collect();
        if cur.col >= chars.len() {
            self.pending.clear();
            return;
        }
        let end = (cur.col + count).min(chars.len());
        let mut new: String = chars[..cur.col].iter().collect();
        for &c in &chars[cur.col..end] {
            let t = if c.is_uppercase() { c.to_lowercase().next().unwrap_or(c) } else { c.to_uppercase().next().unwrap_or(c) };
            new.push(t);
        }
        new.extend(chars[end..].iter());
        self.editor.cur_buffer_mut().text.set_lines(cur.line, cur.line + 1, &[new]);
        self.commit_undo(cur);
        let last = self.editor.cur_buffer().text.line_len(cur.line).saturating_sub(1);
        self.editor.set_cursor(Position::new(cur.line, end.min(last)));
        self.pending.clear();
    }

    /// Apply the pending operator over a text object (`diw`, `ci(`, `ya"`).
    fn apply_textobject(&mut self, around: bool, key: Key) {
        let buf = &self.editor.cur_buffer().text;
        let cur = self.editor.cursor();
        let region = match key {
            Key::Char('w') => textobject::word(buf, cur, around, false),
            Key::Char('W') => textobject::word(buf, cur, around, true),
            Key::Char('(') | Key::Char(')') | Key::Char('b') => textobject::pair(buf, cur, '(', ')', around),
            Key::Char('{') | Key::Char('}') | Key::Char('B') => textobject::pair(buf, cur, '{', '}', around),
            Key::Char('[') | Key::Char(']') => textobject::pair(buf, cur, '[', ']', around),
            Key::Char('<') | Key::Char('>') => textobject::pair(buf, cur, '<', '>', around),
            Key::Char('"') => textobject::quote(buf, cur, '"', around),
            Key::Char('\'') => textobject::quote(buf, cur, '\'', around),
            Key::Char('`') => textobject::quote(buf, cur, '`', around),
            _ => None,
        };
        let Some((start, end)) = region else {
            self.pending.clear();
            return;
        };
        if let Some(op) = self.pending.operator.take() {
            let reg = self.pending.register;
            let span = OperatorSpan { start, end, kind: MotionKind::CharInclusive };
            let outcome = apply_operator(&mut self.editor, op, span, reg);
            self.editor.set_cursor(outcome.cursor);
            if outcome.enter_insert {
                self.start_insert_session();
            }
        }
        self.pending.clear();
    }

    /// `.`: replay the last buffer-changing command's recorded keys.
    fn dot_repeat(&mut self) {
        if self.last_change.is_empty() {
            self.pending.clear();
            return;
        }
        self.pending.clear();
        let keys = self.last_change.clone();
        self.dot_replaying = true;
        for k in keys {
            self.route(k);
        }
        self.dot_replaying = false;
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

        // `g`-prefixed motions (`gg`) — the second key of the pair.
        if self.pending.g_prefix {
            self.pending.g_prefix = false;
            if let Key::Char('g') = key {
                let m = motion::goto_line_first(&self.editor.cur_buffer().text, self.pending.count);
                self.extend_visual(m);
            }
            self.pending.clear();
            return;
        }

        match key {
            Key::Esc => {
                self.record_visual(anchor);
                self.mode = Mode::Normal;
                self.pending.clear();
            }
            // `:` on a selection opens the command line pre-seeded with the
            // visual range, so `:'<,'>` line commands work.
            Key::Char(':') => {
                self.record_visual(anchor);
                self.mode = Mode::Cmdline { prefix: ':', buffer: "'<,'>".to_string() };
                self.pending.clear();
            }
            // Count accumulation, so `3j`, `2w`, `10G`, … extend the selection by
            // a count just like in Normal mode. A leading `0` is the `0` motion.
            Key::Char(c @ '1'..='9') => {
                let d = c.to_digit(10).unwrap() as usize;
                self.pending.count = Some(self.pending.count.unwrap_or(0) * 10 + d);
            }
            Key::Char('0') if self.pending.count.is_some() => {
                self.pending.count = Some(self.pending.count.unwrap() * 10);
            }
            Key::Char('g') => self.pending.g_prefix = true,
            Key::Char('d') | Key::Char('x') => self.visual_operator(Operator::Delete, anchor, kind),
            Key::Char('y') => self.visual_operator(Operator::Yank, anchor, kind),
            Key::Char('c') | Key::Char('s') => self.visual_operator(Operator::Change, anchor, kind),
            Key::Char('>') => self.visual_operator(Operator::Indent { right: true }, anchor, kind),
            Key::Char('<') => self.visual_operator(Operator::Indent { right: false }, anchor, kind),
            Key::Char('u') => self.visual_operator(Operator::Lower, anchor, kind),
            Key::Char('U') => self.visual_operator(Operator::Upper, anchor, kind),
            Key::Char('~') => self.visual_operator(Operator::ToggleCase, anchor, kind),
            other => {
                // Movement extends the selection (honoring any pending count).
                if let Some(m) = self.resolve_motion(other) {
                    self.extend_visual(m);
                }
                self.pending.clear();
            }
        }
    }

    /// Move the cursor (the free end of the selection) to a motion's target,
    /// keeping the current column for linewise motions like `j`/`k` so vertical
    /// movement doesn't snap the column around.
    fn extend_visual(&mut self, m: MotionResult) {
        let target = match m.kind {
            MotionKind::Linewise => Position::new(m.target.line, self.editor.cursor().col),
            _ => m.target,
        };
        self.editor.set_cursor(target);
    }

    /// Record the just-ended Visual selection so `'<` / `'>` addresses resolve.
    fn record_visual(&mut self, anchor: Position) {
        let cursor = self.editor.cursor();
        let (start, end) = if anchor <= cursor { (anchor, cursor) } else { (cursor, anchor) };
        self.last_visual = Some((start, end));
    }

    fn visual_operator(&mut self, op: Operator, anchor: Position, kind: VisualKind) {
        self.record_visual(anchor);
        let cursor = self.editor.cursor();
        let motion_kind = match kind {
            VisualKind::Line => MotionKind::Linewise,
            // Visual char selection is inclusive of the cursor character.
            _ => MotionKind::CharInclusive,
        };
        let span = OperatorSpan::from_motion(&self.editor.cur_buffer().text, anchor, cursor, motion_kind);
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
                match prefix {
                    '/' => self.do_search(&buffer, true),
                    '?' => self.do_search(&buffer, false),
                    _ => self.execute_ex(&buffer),
                }
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

    /// Execute a `:` command line. A leading line range is parsed first; then
    /// range-aware commands (`:s`, `:g`, `:d`, `:m`, …) run in-core, and the
    /// rest fall through to [`parse_ex`] (cursor moves in-core, host effects
    /// queued via [`take_effects`](Self::take_effects)).
    fn execute_ex(&mut self, cmd: &str) {
        let cmd = cmd.trim();
        if cmd.is_empty() {
            return;
        }
        let (spec, rest) = range::parse_range(cmd);

        // A bare range moves the cursor to its last line (`:5`, `:'<,'>`, `:%`).
        if rest.is_empty() {
            if let Some((_, end)) = self.resolve_range(&spec) {
                self.editor.set_cursor(Position::new(end, 0));
            }
            return;
        }

        let (name, arg) = split_ex(&rest);

        // `:!{cmd}` runs a raw command line through the configured shell.
        // Filtering a range through a command (`:%!sort`) isn't supported yet.
        if name == "!" {
            if !matches!(spec, RangeSpec::None) {
                self.queue_effect(ExEffect::Message(
                    "E492: filtering a range through an external command is not supported yet"
                        .into(),
                ));
            } else if arg.is_empty() {
                self.queue_effect(ExEffect::Message("E34: No previous command".into()));
            } else {
                self.queue_effect(ExEffect::Shell(arg));
            }
            return;
        }

        // User commands (`:command Name …`) expand and re-run.
        if let Some(repl) = self.user_commands.get(&name).cloned() {
            let expanded = if arg.is_empty() { repl } else { format!("{repl} {arg}") };
            self.execute_ex(&expanded);
            return;
        }

        if self.run_range_command(&name, &arg, &spec) {
            return;
        }

        if self.run_quickfix_command(&name, &arg) {
            return;
        }

        if self.run_tag_command(&name, &arg) {
            return;
        }

        // Non-range commands (file/quit/buffer/options/…).
        match parse_ex(&rest) {
            ExParsed::GotoLine(line) => {
                let target = line.saturating_sub(1).min(self.editor.cur_buffer().text.line_count() - 1);
                self.editor.set_cursor(Position::new(target, 0));
            }
            ExParsed::GotoLast => {
                let last = self.editor.cur_buffer().text.line_count() - 1;
                self.editor.set_cursor(Position::new(last, 0));
            }
            ExParsed::Undo(n) => {
                for _ in 0..n {
                    self.undo();
                }
            }
            ExParsed::Redo(n) => {
                for _ in 0..n {
                    self.redo();
                }
            }
            ExParsed::Set(items) => self.apply_set(items),
            ExParsed::Map { lhs, rhs } => self.keymap.set_normal(&lhs, &rhs),
            ExParsed::DefineCommand { name, repl } => {
                self.user_commands.insert(name, repl);
            }
            ExParsed::ClearUserCommands => self.user_commands.clear(),
            // Bare `:Find` seeds the panel with the word under the cursor —
            // `parse_ex` is pure, so the fill-in happens here where the buffer
            // is in reach.
            ExParsed::Effect(ExEffect::OpenReplace { pattern: None }) => {
                let pattern = self.word_at_cursor();
                self.queue_effect(ExEffect::OpenReplace { pattern });
            }
            ExParsed::Effect(effect) => self.queue_effect(effect),
            ExParsed::Nop => {}
        }
    }

    /// Dispatch a quickfix command (`:copen`, `:cnext`, `:make`, …), returning
    /// whether it was one.
    ///
    /// Navigation happens here because the engine owns the list; the resulting
    /// jump, and anything needing the filesystem or a process, goes out as an
    /// [`ExEffect::Quickfix`] for the host.
    fn run_quickfix_command(&mut self, name: &str, arg: &str) -> bool {
        let count = arg.trim().parse::<usize>().ok();
        match name {
            "copen" | "cope" | "cw" | "cwindow" => {
                self.queue_effect(ExEffect::Quickfix(QuickfixCmd::Open));
            }
            "cclose" | "ccl" => self.queue_effect(ExEffect::Quickfix(QuickfixCmd::Close)),
            "cnext" | "cn" => self.quickfix_step(count.unwrap_or(1) as isize),
            "cprevious" | "cprev" | "cp" | "cN" => {
                self.quickfix_step(-(count.unwrap_or(1) as isize))
            }
            "cfirst" | "cfir" | "crewind" | "cr" => self.quickfix_end(false),
            "clast" | "cla" => self.quickfix_end(true),
            "cc" => {
                // `:cc N` is 1-based on the command line, 0-based in the list.
                let target = count.map(|n| n.saturating_sub(1));
                let jump = match target {
                    Some(i) => self.editor.quickfix.goto(i).cloned(),
                    None => self.editor.quickfix.current().cloned(),
                };
                self.quickfix_jump(jump);
            }
            "clist" | "cl" => {
                let qf = &self.editor.quickfix;
                let msg = if qf.is_empty() {
                    "E42: no errors".to_string()
                } else {
                    format!("{} entries: {}", qf.len(), qf.title())
                };
                self.queue_effect(ExEffect::Message(msg));
            }
            "vimgrep" | "vim" | "vimg" => match parse_vimgrep(arg) {
                Some((pattern, glob)) => {
                    self.queue_effect(ExEffect::Quickfix(QuickfixCmd::Grep { pattern, glob }))
                }
                None => self.queue_effect(ExEffect::Message(
                    "E682: usage: :vimgrep /pattern/ [glob]".into(),
                )),
            },
            "make" => self.queue_effect(ExEffect::Quickfix(QuickfixCmd::Run {
                program: "cargo".into(),
                args: {
                    let mut args = vec!["build".to_string()];
                    args.extend(arg.split_whitespace().map(str::to_string));
                    args
                },
                title: ":make".into(),
            })),
            "grep" | "gr" => {
                let words: Vec<String> = arg.split_whitespace().map(str::to_string).collect();
                if words.is_empty() {
                    self.queue_effect(ExEffect::Message("E683: usage: :grep {pattern}".into()));
                } else {
                    let mut args = vec!["-rn".to_string()];
                    args.extend(words);
                    self.queue_effect(ExEffect::Quickfix(QuickfixCmd::Run {
                        program: "grep".into(),
                        args,
                        title: format!(":grep {}", arg.trim()),
                    }));
                }
            }
            _ => return false,
        }
        true
    }

    /// `:cnext`/`:cprev` — move within the list and jump, or report why not.
    fn quickfix_step(&mut self, by: isize) {
        let item = self.editor.quickfix.advance(by).cloned();
        self.quickfix_jump(item);
    }

    /// `:cfirst`/`:clast`.
    fn quickfix_end(&mut self, last: bool) {
        let item = self.editor.quickfix.goto_end(last).cloned();
        self.quickfix_jump(item);
    }

    /// Turn a selected entry into a host jump, or an empty-list message.
    fn quickfix_jump(&mut self, item: Option<crate::quickfix::QfItem>) {
        let effect = match item {
            Some(item) => ExEffect::Quickfix(QuickfixCmd::Jump {
                path: item.path.to_string_lossy().into_owned(),
                line: item.line,
                col: item.col,
            }),
            None => ExEffect::Message("E42: no errors".into()),
        };
        self.queue_effect(effect);
    }

    // --- tags ---

    /// Install a parsed tags file (the host reads it; see [`ExEffect::Tag`]).
    pub fn set_tags(&mut self, table: crate::tags::TagTable) {
        self.editor.tags = table;
    }

    pub fn tags(&self) -> &crate::tags::TagTable {
        &self.editor.tags
    }

    pub fn tagstack(&self) -> &crate::tags::TagStack {
        &self.editor.tagstack
    }

    /// The identifier under the cursor — what `Ctrl-]` looks up.
    ///
    /// Keyword characters are alphanumerics and `_`, and the word extends both
    /// ways from the cursor, so the cursor may sit anywhere inside it.
    pub fn word_at_cursor(&self) -> Option<String> {
        let cursor = self.editor.cursor();
        let line = self.editor.cur_buffer().text.line(cursor.line)?;
        let chars: Vec<char> = line.chars().collect();
        let is_word = |c: char| c.is_alphanumeric() || c == '_';
        if chars.is_empty() {
            return None;
        }
        let at = cursor.col.min(chars.len() - 1);
        if !is_word(chars[at]) {
            return None;
        }
        let start = chars[..at].iter().rposition(|&c| !is_word(c)).map_or(0, |i| i + 1);
        let end = chars[at..]
            .iter()
            .position(|&c| !is_word(c))
            .map_or(chars.len(), |i| at + i);
        Some(chars[start..end].iter().collect())
    }

    /// Record the current position on the tagstack and select `name`'s first
    /// definition. Called by the host once it has the tags file loaded.
    ///
    /// Returns the tag to jump to, or `None` when the name isn't in the table
    /// (the host reports `E426`).
    pub fn select_tag(&mut self, name: &str, from_path: &str) -> Option<crate::tags::Tag> {
        let matches = self.editor.tags.find(name).to_vec();
        if matches.is_empty() {
            return None;
        }
        let cursor = self.editor.cursor();
        self.editor.tagstack.push(crate::tags::TagStackEntry {
            name: name.to_string(),
            path: from_path.to_string(),
            line: cursor.line,
            col: cursor.col,
        });
        let first = matches[0].clone();
        self.editor.tag_matches = Some(crate::tags::TagMatches {
            name: name.to_string(),
            matches,
            current: 0,
        });
        Some(first)
    }

    /// How many definitions the last lookup found (for the `tag 1 of 3` note).
    pub fn tag_match_count(&self) -> usize {
        self.editor.tag_matches.as_ref().map_or(0, |m| m.matches.len())
    }

    /// `:tnext` / `:tprev` — move within the current match list, reporting at
    /// the ends rather than re-jumping to the same definition.
    fn tag_step(&mut self, by: isize) {
        let moved = self
            .editor
            .tag_matches
            .as_mut()
            .map(|m| (m.advance(by).cloned(), m.current + 1, m.matches.len()));
        self.tag_jump(moved);
    }

    /// `:tfirst` / `:tlast` — land on that end of the match list, which always
    /// works when there is one.
    fn tag_end(&mut self, last: bool) {
        let moved = self
            .editor
            .tag_matches
            .as_mut()
            .map(|m| (m.goto_end(last).cloned(), m.current + 1, m.matches.len()));
        self.tag_jump(moved);
    }

    /// Emit the jump for a match-list move, or the reason there wasn't one.
    fn tag_jump(&mut self, moved: Option<(Option<crate::tags::Tag>, usize, usize)>) {
        match moved {
            Some((Some(tag), index, total)) => {
                self.queue_effect(ExEffect::Tag(TagCmd::Jump {
                    path: tag.path,
                    address: tag.address,
                }));
                self.queue_effect(ExEffect::Message(format!("tag {index} of {total}")));
            }
            Some((None, _, _)) => {
                self.queue_effect(ExEffect::Message("E425: no more matching tags".into()))
            }
            None => self.queue_effect(ExEffect::Message("E73: tag stack empty".into())),
        }
    }

    /// `Ctrl-T` — return to where the last `Ctrl-]` jumped from.
    fn tag_pop(&mut self) {
        match self.editor.tagstack.pop() {
            Some(entry) => self.queue_effect(ExEffect::Tag(TagCmd::Return {
                path: entry.path,
                line: entry.line,
                col: entry.col,
            })),
            None => self.queue_effect(ExEffect::Message("E73: tag stack empty".into())),
        }
    }

    /// Ask the host to look a name up (it refreshes the tags file first).
    fn tag_lookup(&mut self, name: Option<String>) {
        let name = name.or_else(|| self.word_at_cursor());
        match name {
            Some(name) if !name.is_empty() => {
                self.queue_effect(ExEffect::Tag(TagCmd::Lookup { name }))
            }
            _ => self.queue_effect(ExEffect::Message("E349: no identifier under cursor".into())),
        }
    }

    /// Dispatch a tag command (`:tag`, `:tnext`, `:tags`, …). Returns whether
    /// it was one.
    fn run_tag_command(&mut self, name: &str, arg: &str) -> bool {
        let arg = arg.trim();
        match name {
            "ta" | "tag" => self.tag_lookup((!arg.is_empty()).then(|| arg.to_string())),
            "tn" | "tnext" => self.tag_step(1),
            "tp" | "tprevious" | "tprev" | "tN" => self.tag_step(-1),
            "tf" | "tfirst" | "tr" | "trewind" => self.tag_end(false),
            "tl" | "tlast" => self.tag_end(true),
            "po" | "pop" => self.tag_pop(),
            "ts" | "tselect" | "tj" | "tjump" => {
                // With one match these behave like `:tag`; the selection UI is
                // not built, so report the alternatives instead of prompting.
                let target = if arg.is_empty() { self.word_at_cursor() } else { Some(arg.to_string()) };
                match target {
                    Some(name) => {
                        let matches = self.editor.tags.find(&name);
                        if matches.len() > 1 {
                            let list: Vec<String> = matches
                                .iter()
                                .enumerate()
                                .map(|(i, t)| format!("{}: {}", i + 1, t.path))
                                .collect();
                            let msg = format!("{} matches — {}", matches.len(), list.join("  "));
                            self.queue_effect(ExEffect::Message(msg));
                        }
                        self.tag_lookup(Some(name));
                    }
                    None => self.queue_effect(ExEffect::Message(
                        "E349: no identifier under cursor".into(),
                    )),
                }
            }
            "tags" => {
                let stack = self.editor.tagstack.entries();
                let msg = if stack.is_empty() {
                    "tag stack empty".to_string()
                } else {
                    let items: Vec<String> = stack
                        .iter()
                        .enumerate()
                        .map(|(i, e)| format!("{} {} {}:{}", i + 1, e.name, e.path, e.line + 1))
                        .collect();
                    items.join("  ")
                };
                self.queue_effect(ExEffect::Message(msg));
            }
            _ => return false,
        }
        true
    }

    // --- folds ---

    /// The current window's folds.
    pub fn folds(&self) -> &crate::fold::Folds {
        &self.editor.window(self.editor.current_window_id()).unwrap().folds
    }

    fn folds_mut(&mut self) -> &mut crate::fold::Folds {
        let id = self.editor.current_window_id();
        &mut self.editor.window_mut(id).unwrap().folds
    }

    /// Handle the `z` fold commands. Returns whether the key was one of them.
    ///
    /// `zf` is an operator (it awaits a motion, `zfj` / `zf}`); the rest act on
    /// the fold under the cursor immediately.
    fn fold_key(&mut self, key: Key) -> bool {
        let line = self.editor.cursor().line;
        match key {
            // `zf{motion}` — the motion decides the range, so this waits.
            Key::Char('f') => {
                self.pending.operator = Some(Operator::Fold);
                return true;
            }
            Key::Char('a') => {
                self.folds_mut().set_closed_at(line, None);
            }
            Key::Char('o') => {
                self.folds_mut().set_closed_at(line, Some(false));
            }
            Key::Char('c') => {
                self.folds_mut().set_closed_at(line, Some(true));
            }
            Key::Char('R') => self.folds_mut().set_all_closed(false),
            Key::Char('M') => self.folds_mut().set_all_closed(true),
            Key::Char('d') => self.folds_mut().delete_at(line),
            Key::Char('E') => self.folds_mut().clear(),
            Key::Char('i') => {
                let on = self.folds().enabled;
                self.folds_mut().enabled = !on;
            }
            // `zj`/`zk` — move to the start/end of the next/previous fold.
            Key::Char('j') | Key::Char('k') => {
                let forward = matches!(key, Key::Char('j'));
                if let Some(target) = self.next_fold_edge(line, forward) {
                    self.editor.set_cursor(Position::new(target, 0));
                }
            }
            _ => return false,
        }
        // A fold may have closed under the cursor; keep the cursor on a line
        // that is actually visible.
        self.snap_cursor_out_of_fold();
        true
    }

    /// The nearest fold boundary after/before `line`, for `zj`/`zk`.
    fn next_fold_edge(&self, line: usize, forward: bool) -> Option<usize> {
        let folds = self.folds();
        if forward {
            folds.all().iter().map(|f| f.start).filter(|&s| s > line).min()
        } else {
            folds.all().iter().map(|f| f.end).filter(|&e| e < line).max()
        }
    }

    /// Move the cursor to a visible line if a fold just swallowed it — Vim puts
    /// the cursor on the fold's first line when you close one around it.
    fn snap_cursor_out_of_fold(&mut self) {
        let cursor = self.editor.cursor();
        if let Some(fold) = self.folds().closed_at(cursor.line) {
            if fold.start != cursor.line {
                let start = fold.start;
                self.editor.set_cursor(Position::new(start, cursor.col));
            }
        }
    }

    /// Create a fold over a line range (`zf{motion}`, `:fold`).
    pub fn create_fold(&mut self, start: usize, end: usize) {
        self.folds_mut().create(start, end);
        self.snap_cursor_out_of_fold();
    }

    /// Re-derive folds when `'foldmethod'` computes them. Called after edits;
    /// a no-op for `manual`.
    pub fn refresh_folds(&mut self) {
        let method = self.editor.options().foldmethod();
        if method == ctrlvim_options::FoldMethod::Manual {
            return;
        }
        let shiftwidth = self.editor.options().shiftwidth().max(1) as usize;
        let enabled = self.editor.options().foldenable();
        let lines = self.editor.cur_buffer().text.lines();
        let folds = self.folds_mut();
        folds.enabled = enabled;
        folds.recompute(method, &lines, shiftwidth);
    }

    /// Replace the quickfix list — called by the host once it has walked files
    /// (`:vimgrep`) or collected a program's output (`:make`, `:grep`).
    pub fn set_quickfix(&mut self, items: Vec<crate::quickfix::QfItem>, title: impl Into<String>) {
        self.editor.quickfix.set(items, title);
    }

    /// The quickfix list, for the host to render.
    pub fn quickfix(&self) -> &crate::quickfix::QuickfixList {
        &self.editor.quickfix
    }

    /// User-defined commands (`:command Name expansion`), name and expansion
    /// pairs, for the host to list in the command palette.
    pub fn user_commands(&self) -> impl Iterator<Item = (&str, &str)> {
        self.user_commands.iter().map(|(name, repl)| (name.as_str(), repl.as_str()))
    }

    /// Select entry `index` (0-based) and return the jump for the host — what
    /// clicking a row in the quickfix pane does.
    pub fn quickfix_select(&mut self, index: usize) -> Option<crate::quickfix::QfItem> {
        self.editor.quickfix.goto(index).cloned()
    }

    /// Dispatch a range-aware command, returning whether it was one. Each picks
    /// its default range (current line, or the whole file for `:g`/`:sort`).
    fn run_range_command(&mut self, name: &str, arg: &str, spec: &RangeSpec) -> bool {
        match name {
            "s" | "su" | "sub" | "substitute" => {
                let r = self.range_or_current(spec);
                self.ex_substitute(r, arg);
            }
            "g" | "global" => {
                let (inv, body) = match arg.strip_prefix('!') {
                    Some(b) => (true, b),
                    None => (false, arg),
                };
                let r = self.range_or_whole(spec);
                self.ex_global(r, body, inv);
            }
            "v" | "vglobal" => {
                let r = self.range_or_whole(spec);
                self.ex_global(r, arg, true);
            }
            "d" | "de" | "del" | "delete" => {
                let r = self.range_or_current(spec);
                self.ex_delete(r);
            }
            "y" | "ya" | "yank" => {
                let r = self.range_or_current(spec);
                self.ex_yank(r);
            }
            // `:{range}fold` and the fold open/close commands, which take a
            // range like every other Ex command (`:1,20fold`, `:%foldclose`).
            "fo" | "fold" => {
                let (start, end) = self.range_or_current(spec);
                self.create_fold(start, end);
            }
            "foldo" | "foldopen" => {
                let (start, end) = self.range_or_current(spec);
                for line in start..=end {
                    self.folds_mut().set_closed_at(line, Some(false));
                }
            }
            "foldc" | "foldclose" => {
                let (start, end) = self.range_or_current(spec);
                for line in start..=end {
                    self.folds_mut().set_closed_at(line, Some(true));
                }
                self.snap_cursor_out_of_fold();
            }
            "m" | "mo" | "move" => {
                let r = self.range_or_current(spec);
                self.ex_move(r, arg);
            }
            "t" | "co" | "copy" => {
                let r = self.range_or_current(spec);
                self.ex_copy(r, arg);
            }
            "j" | "join" => {
                let r = self.range_or_current(spec);
                self.ex_join(r);
            }
            ">" => {
                let r = self.range_or_current(spec);
                self.ex_shift(r, true);
            }
            "<" => {
                let r = self.range_or_current(spec);
                self.ex_shift(r, false);
            }
            "sort" | "sor" => {
                let r = self.range_or_whole(spec);
                self.ex_sort(r, arg);
            }
            "normal" | "norm" => {
                let r = self.resolve_range(spec);
                self.ex_normal(r, arg);
            }
            "pu" | "put" => self.ex_put(spec, arg),
            "noh" | "nohl" | "nohlsearch" => self.search_highlight = false,
            _ => return false,
        }
        true
    }

    // --- range resolution ---

    /// Resolve a single address to a 0-based line, clamped to the buffer.
    fn resolve_address(&self, a: &Address) -> Option<usize> {
        let last = self.editor.cur_buffer().text.line_count().saturating_sub(1);
        let base = match &a.base {
            Addr::Current => self.editor.cursor().line,
            Addr::Last => last,
            Addr::Line(n) => n.saturating_sub(1),
            Addr::Mark('<') => self.last_visual?.0.line,
            Addr::Mark('>') => self.last_visual?.1.line,
            Addr::Mark(_) => return None,
            Addr::Search { pattern, forward } => self.find_match(pattern, *forward)?.line,
        };
        Some((base as i64 + a.offset).clamp(0, last as i64) as usize)
    }

    /// Resolve a range spec to an inclusive `(start, end)` line pair.
    fn resolve_range(&self, r: &RangeSpec) -> Option<(usize, usize)> {
        let last = self.editor.cur_buffer().text.line_count().saturating_sub(1);
        match r {
            RangeSpec::None => None,
            RangeSpec::Whole => Some((0, last)),
            RangeSpec::One(a) => self.resolve_address(a).map(|l| (l, l)),
            RangeSpec::Pair(a, b) => {
                let s = self.resolve_address(a)?;
                let e = self.resolve_address(b)?;
                Some((s.min(e), s.max(e)))
            }
        }
    }

    /// The range, defaulting to the current line when none was given.
    fn range_or_current(&self, r: &RangeSpec) -> (usize, usize) {
        self.resolve_range(r).unwrap_or_else(|| {
            let l = self.editor.cursor().line;
            (l, l)
        })
    }

    /// The range, defaulting to the whole file when none was given.
    fn range_or_whole(&self, r: &RangeSpec) -> (usize, usize) {
        let last = self.editor.cur_buffer().text.line_count().saturating_sub(1);
        self.resolve_range(r).unwrap_or((0, last))
    }

    // --- search ---

    /// Run a `/`/`?` search: remember the pattern and jump to the next match.
    fn do_search(&mut self, pattern: &str, forward: bool) {
        let pattern = if pattern.is_empty() {
            match &self.last_search {
                Some((p, _)) => p.clone(),
                None => return,
            }
        } else {
            pattern.to_string()
        };
        self.last_search = Some((pattern.clone(), forward));
        self.search_highlight = true;
        self.jump_to_search(&pattern, forward);
    }

    /// `n` (same direction) / `N` (reversed) — repeat the last search.
    fn search_next(&mut self, same_dir: bool) {
        let Some((pattern, fwd)) = self.last_search.clone() else { return };
        let forward = if same_dir { fwd } else { !fwd };
        self.search_highlight = true;
        self.jump_to_search(&pattern, forward);
    }

    /// Move the cursor to the next match of `pattern` from the cursor, wrapping.
    fn jump_to_search(&mut self, pattern: &str, forward: bool) {
        if let Some(pos) = self.find_match(pattern, forward) {
            self.editor.set_cursor(pos);
        } else {
            self.effects.push(ExEffect::Message(format!("E486: Pattern not found: {pattern}")));
        }
    }

    /// Find the next/previous match of a Vim `pattern` from the cursor (with
    /// wrap-around). Returns the match's start position.
    fn find_match(&self, pattern: &str, forward: bool) -> Option<Position> {
        let re = compile_pattern(pattern).ok()?;
        let text = &self.editor.cur_buffer().text;
        let n = text.line_count();
        let cur = self.editor.cursor();
        // All match start positions, in buffer order.
        let mut matches: Vec<Position> = Vec::new();
        for line in 0..n {
            let s = text.line(line).unwrap_or_default();
            for m in re.find_iter(&s) {
                matches.push(Position::new(line, m.start()));
            }
        }
        if matches.is_empty() {
            return None;
        }
        if forward {
            matches
                .iter()
                .find(|p| **p > cur)
                .or_else(|| matches.first())
                .copied()
        } else {
            matches
                .iter()
                .rev()
                .find(|p| **p < cur)
                .or_else(|| matches.last())
                .copied()
        }
    }

    /// Char-column `(start, end)` ranges of the active search pattern's matches
    /// on `line`, for the frontend's `hlsearch` highlighting. Empty when search
    /// highlighting is off (`:noh`) or there's no pattern.
    pub fn search_line_matches(&self, line: usize) -> Vec<(usize, usize)> {
        if !self.search_highlight {
            return Vec::new();
        }
        let Some((pattern, _)) = &self.last_search else { return Vec::new() };
        let Ok(re) = compile_pattern(pattern) else { return Vec::new() };
        let Some(text) = self.editor.cur_buffer().text.line(line) else { return Vec::new() };
        re.find_iter(&text)
            .filter(|m| m.start() != m.end()) // skip empty matches
            .map(|m| {
                let start = text[..m.start()].chars().count();
                let end = text[..m.end()].chars().count();
                (start, end)
            })
            .collect()
    }

    // --- range-aware line commands ---

    /// `:[range]s/pat/rep/flags` — substitute on each line of the range.
    fn ex_substitute(&mut self, range: (usize, usize), args: &str) {
        let (pat, rep, flags) = match split_subst(args) {
            Some(parts) => parts,
            None => match &self.last_subst {
                Some(prev) => prev.clone(),
                None => return,
            },
        };
        let pattern = if pat.is_empty() {
            match &self.last_search {
                Some((p, _)) => p.clone(),
                None => return,
            }
        } else {
            pat.clone()
        };
        self.last_search = Some((pattern.clone(), true));
        self.last_subst = Some((pattern.clone(), rep.clone(), flags.clone()));

        let ignorecase = flags.contains('i');
        let global = flags.contains('g');
        let re = match compile_pattern_opts(&pattern, ignorecase) {
            Ok(re) => re,
            Err(_) => {
                self.effects.push(ExEffect::Message(format!("E486: Pattern error: {pattern}")));
                return;
            }
        };
        let replacement = vim_replacement(&rep);

        let (start, end) = range;
        let text = &self.editor.cur_buffer().text;
        let last = text.line_count().saturating_sub(1);
        let end = end.min(last);
        let mut out: Vec<String> = Vec::new();
        let mut changed = 0usize;
        let mut last_hit = start;
        for i in start..=end {
            let line = text.line(i).unwrap_or_default();
            if re.is_match(&line) {
                changed += 1;
                last_hit = i;
                let new = if global {
                    re.replace_all(&line, replacement.as_str())
                } else {
                    re.replace(&line, replacement.as_str())
                };
                for piece in new.split('\n') {
                    out.push(piece.to_string());
                }
            } else {
                out.push(line);
            }
        }
        if changed == 0 {
            self.effects.push(ExEffect::Message(format!("E486: Pattern not found: {pattern}")));
            return;
        }
        self.editor.cur_buffer_mut().text.set_lines(start, end + 1, &out);
        let cursor = Position::new(last_hit.min(self.editor.cur_buffer().text.line_count().saturating_sub(1)), 0);
        self.editor.set_cursor(cursor);
        self.commit_undo(cursor);
    }

    /// `:[range]g/pat/cmd` (or `:v`/`:g!`) — run `cmd` on each line matching
    /// (or, inverted, not matching) `pat`. Marks the lines first, then applies
    /// the command bottom-to-top so deletions don't shift pending lines.
    fn ex_global(&mut self, range: (usize, usize), args: &str, invert: bool) {
        let (pattern, cmd) = match split_global(args) {
            Some(pc) => pc,
            None => return,
        };
        let cmd = if cmd.trim().is_empty() { "p".to_string() } else { cmd };
        let re = match compile_pattern(&pattern) {
            Ok(re) => re,
            Err(_) => {
                self.effects.push(ExEffect::Message(format!("E486: Pattern error: {pattern}")));
                return;
            }
        };
        let (start, end) = range;
        let text = &self.editor.cur_buffer().text;
        let last = text.line_count().saturating_sub(1);
        let end = end.min(last);
        // Collect matching line numbers (as content, since indices shift).
        let mut targets: Vec<usize> = Vec::new();
        for i in start..=end {
            let line = text.line(i).unwrap_or_default();
            if re.is_match(&line) != invert {
                targets.push(i);
            }
        }
        // Apply bottom-up so earlier line numbers stay valid across edits.
        for &line in targets.iter().rev() {
            self.editor.set_cursor(Position::new(line, 0));
            let (name, cmd_arg) = split_ex(cmd.trim());
            let one = RangeSpec::One(Address { base: Addr::Line(line + 1), offset: 0 });
            if !self.run_range_command(&name, &cmd_arg, &one) {
                // Non-range command inside :g (e.g. bare `p`); ignore quietly.
            }
        }
    }

    /// `:[range]d` — delete the range's lines into the unnamed register.
    fn ex_delete(&mut self, (start, end): (usize, usize)) {
        let text = &self.editor.cur_buffer().text;
        let last = text.line_count().saturating_sub(1);
        let end = end.min(last);
        let lines: Vec<String> = (start..=end).map(|i| text.line(i).unwrap_or_default()).collect();
        self.editor.registers.delete(None, YankReg::new(lines, MotionType::Line));
        let empty: [String; 0] = [];
        self.editor.cur_buffer_mut().text.set_lines(start, end + 1, &empty);
        // Never leave a zero-line buffer.
        if self.editor.cur_buffer().text.line_count() == 0 {
            self.editor.cur_buffer_mut().text.set_lines(0, 0, &[String::new()]);
        }
        let new_last = self.editor.cur_buffer().text.line_count().saturating_sub(1);
        let cursor = Position::new(start.min(new_last), 0);
        self.editor.set_cursor(cursor);
        self.commit_undo(cursor);
    }

    /// `:[range]y` — yank the range's lines into the unnamed register.
    fn ex_yank(&mut self, (start, end): (usize, usize)) {
        let text = &self.editor.cur_buffer().text;
        let last = text.line_count().saturating_sub(1);
        let end = end.min(last);
        let lines: Vec<String> = (start..=end).map(|i| text.line(i).unwrap_or_default()).collect();
        self.editor.registers.yank(None, YankReg::new(lines, MotionType::Line));
    }

    /// `:[range]m {addr}` — move the range's lines to after `{addr}`.
    fn ex_move(&mut self, (start, end): (usize, usize), arg: &str) {
        let Some(dest) = self.resolve_dest(arg) else { return };
        let text = &self.editor.cur_buffer().text;
        let last = text.line_count().saturating_sub(1);
        let end = end.min(last);
        let block: Vec<String> = (start..=end).map(|i| text.line(i).unwrap_or_default()).collect();
        let count = block.len();
        // `dest` is the 0-based line to insert *after* in the original buffer
        // (`-1` = above line 1). After removing the block, a destination below
        // it shifts up by `count`.
        let insert_after = dest + 1; // 1-based position "below dest"
        let empty: [String; 0] = [];
        self.editor.cur_buffer_mut().text.set_lines(start, end + 1, &empty);
        let insert_at = if dest >= end as i64 + 1 {
            (insert_after - count as i64).max(0)
        } else {
            insert_after.max(0)
        } as usize;
        let insert_at = insert_at.min(self.editor.cur_buffer().text.line_count());
        self.editor.cur_buffer_mut().text.set_lines(insert_at, insert_at, &block);
        let cursor = Position::new(insert_at + count - 1, 0);
        self.editor.set_cursor(cursor);
        self.commit_undo(cursor);
    }

    /// `:[range]t {addr}` / `:copy` — copy the range's lines to after `{addr}`.
    fn ex_copy(&mut self, (start, end): (usize, usize), arg: &str) {
        let Some(dest) = self.resolve_dest(arg) else { return };
        let text = &self.editor.cur_buffer().text;
        let last = text.line_count().saturating_sub(1);
        let end = end.min(last);
        let block: Vec<String> = (start..=end).map(|i| text.line(i).unwrap_or_default()).collect();
        let count = block.len();
        let insert_at = ((dest + 1).max(0) as usize).min(self.editor.cur_buffer().text.line_count());
        self.editor.cur_buffer_mut().text.set_lines(insert_at, insert_at, &block);
        let cursor = Position::new(insert_at + count - 1, 0);
        self.editor.set_cursor(cursor);
        self.commit_undo(cursor);
    }

    /// Resolve a `:m`/`:t` destination to the 0-based line to insert *after*
    /// (`-1` = above the first line, from the special address `0`).
    fn resolve_dest(&self, arg: &str) -> Option<i64> {
        let arg = arg.trim();
        if arg == "0" {
            return Some(-1);
        }
        let (spec, _) = range::parse_range(arg);
        match spec {
            RangeSpec::One(a) | RangeSpec::Pair(_, a) => self.resolve_address(&a).map(|l| l as i64),
            _ => None,
        }
    }

    /// `:[range]j` — join the range's lines into one (single spaces).
    fn ex_join(&mut self, (start, end): (usize, usize)) {
        let text = &self.editor.cur_buffer().text;
        let last = text.line_count().saturating_sub(1);
        let end = end.min(last).max(start + 1).min(last);
        if end <= start {
            return;
        }
        let joined = (start..=end)
            .map(|i| text.line(i).unwrap_or_default())
            .enumerate()
            .map(|(k, l)| if k == 0 { l } else { l.trim_start().to_string() })
            .collect::<Vec<_>>()
            .join(" ");
        self.editor.cur_buffer_mut().text.set_lines(start, end + 1, &[joined]);
        let cursor = Position::new(start, 0);
        self.editor.set_cursor(cursor);
        self.commit_undo(cursor);
    }

    /// `:[range]>` / `:<` — shift the range's lines by one `shiftwidth`.
    fn ex_shift(&mut self, (start, end): (usize, usize), right: bool) {
        let sw = self.editor.global_options.shiftwidth.max(0) as usize;
        let sw = if sw == 0 { self.editor.global_options.tabstop.max(1) as usize } else { sw };
        let text = &self.editor.cur_buffer().text;
        let last = text.line_count().saturating_sub(1);
        let end = end.min(last);
        let new: Vec<String> = (start..=end)
            .map(|i| {
                let line = text.line(i).unwrap_or_default();
                if right {
                    if line.is_empty() {
                        line
                    } else {
                        format!("{}{}", " ".repeat(sw), line)
                    }
                } else {
                    let trimmed = line.trim_start_matches(' ');
                    let removed = line.len() - trimmed.len();
                    line[removed.min(sw)..].to_string()
                }
            })
            .collect();
        self.editor.cur_buffer_mut().text.set_lines(start, end + 1, &new);
        let cursor = Position::new(start, 0);
        self.editor.set_cursor(cursor);
        self.commit_undo(cursor);
    }

    /// `:[range]sort` — sort the range's lines. `!` reverses; `n` sorts numeric;
    /// `u` removes duplicates; `i` ignores case.
    fn ex_sort(&mut self, (start, end): (usize, usize), flags: &str) {
        let reverse = flags.contains('!') || flags.contains('r');
        let numeric = flags.contains('n');
        let ignorecase = flags.contains('i');
        let unique = flags.contains('u');
        let text = &self.editor.cur_buffer().text;
        let last = text.line_count().saturating_sub(1);
        let end = end.min(last);
        let mut lines: Vec<String> = (start..=end).map(|i| text.line(i).unwrap_or_default()).collect();
        if numeric {
            lines.sort_by_key(|l| l.trim().parse::<i64>().unwrap_or(i64::MIN));
        } else if ignorecase {
            lines.sort_by_key(|l| l.to_lowercase());
        } else {
            lines.sort();
        }
        if unique {
            lines.dedup();
        }
        if reverse {
            lines.reverse();
        }
        self.editor.cur_buffer_mut().text.set_lines(start, end + 1, &lines);
        let cursor = Position::new(start, 0);
        self.editor.set_cursor(cursor);
        self.commit_undo(cursor);
    }

    /// `:[range]normal {keys}` — run `keys` as Normal-mode commands, once per
    /// line of the range (or once at the cursor when no range is given).
    fn ex_normal(&mut self, range: Option<(usize, usize)>, keys: &str) {
        let keys: Vec<Key> = Key::parse_sequence(keys);
        let run = |s: &mut Self| {
            for key in keys.iter().copied() {
                s.feed(key);
            }
            // Leave any lingering Insert/Cmdline mode (Vim appends an <Esc>).
            if !matches!(s.mode, Mode::Normal) {
                s.feed(Key::Esc);
            }
        };
        match range {
            None => run(self),
            Some((start, end)) => {
                let mut i = start;
                while i <= end && i < self.editor.cur_buffer().text.line_count() {
                    self.editor.set_cursor(Position::new(i, 0));
                    run(self);
                    i += 1;
                }
            }
        }
    }

    /// `:[line]put [x]` — put register `x` (default unnamed) below the address.
    fn ex_put(&mut self, spec: &RangeSpec, arg: &str) {
        let at = match self.resolve_range(spec) {
            Some((_, end)) => end,
            None => self.editor.cursor().line,
        };
        let name = arg.trim().chars().next().unwrap_or('"');
        let Some(reg) = self.editor.registers.read(name).cloned() else { return };
        let insert_at = (at + 1).min(self.editor.cur_buffer().text.line_count());
        self.editor.cur_buffer_mut().text.set_lines(insert_at, insert_at, &reg.lines);
        let cursor = Position::new(insert_at, 0);
        self.editor.set_cursor(cursor);
        self.commit_undo(cursor);
    }

    /// Apply `:set` option changes to the editor's global options, reporting the
    /// first unknown option as an error message (like Neovim's `E518`).
    fn apply_set(&mut self, items: Vec<crate::ex::SetItem>) {
        use crate::ex::SetItem;
        for item in items {
            // Each arm re-borrows rather than holding `global_options` across
            // the loop, since the fold options also touch window state.
            let opts = &mut self.editor.global_options;
            match item {
                SetItem::Number(v) => opts.number = v,
                SetItem::Wrap(v) => opts.wrap = v,
                SetItem::Expandtab(v) => opts.expandtab = v,
                SetItem::Tabstop(n) => opts.tabstop = n.max(1),
                SetItem::Shiftwidth(n) => opts.shiftwidth = n.max(0),
                SetItem::Scrolloff(n) => opts.scrolloff = n.max(0),
                SetItem::Foldcolumn(n) => opts.foldcolumn = n.clamp(0, 9),
                SetItem::Foldenable(v) => {
                    opts.foldenable = v;
                    self.folds_mut().enabled = v;
                }
                SetItem::Foldmethod(m) => opts.foldmethod = m,
                SetItem::Unknown(name) => {
                    self.effects.push(ExEffect::Message(format!(
                        "E518: Unknown option: {name}"
                    )));
                }
            }
        }
        // Several of these feed `foldmethod=indent` (the method itself, but
        // also `shiftwidth`, which decides what counts as one level), so
        // re-derive once at the end rather than per option. No-op for `manual`.
        self.refresh_folds();
    }

    /// Queue an Ex effect, applying the modified-buffer semantics the engine
    /// owns: `:w`/`:wq` mark the buffer saved; `:q` on an unsaved buffer is
    /// refused (unless `!`) with an error rather than a quit.
    fn queue_effect(&mut self, effect: ExEffect) {
        match effect {
            ExEffect::Write { .. } | ExEffect::WriteQuit { .. } => {
                self.editor.cur_buffer_mut().mark_saved();
                self.effects.push(effect);
            }
            ExEffect::Quit { force } if !force && self.editor.cur_buffer().modified() => {
                self.effects.push(ExEffect::Message(
                    "E37: No write since last change (add ! to override)".into(),
                ));
            }
            other => self.effects.push(other),
        }
    }

    /// Whether the current buffer has unsaved changes.
    pub fn is_modified(&self) -> bool {
        self.editor.cur_buffer().modified()
    }

    /// Force the current buffer's modified state (used by the host to carry
    /// per-buffer dirty state across the single-buffer facade). `true` bumps
    /// `changedtick` past the saved mark; `false` marks it saved.
    pub fn set_modified(&mut self, modified: bool) {
        if modified {
            if !self.editor.cur_buffer().modified() {
                self.editor.cur_buffer_mut().changedtick += 1;
            }
        } else {
            self.editor.cur_buffer_mut().mark_saved();
        }
    }

    /// Drain the host effects requested since the last call (`:w`/`:q`/…).
    pub fn take_effects(&mut self) -> Vec<ExEffect> {
        std::mem::take(&mut self.effects)
    }

    /// The in-progress command line (`prefix + typed text`) while in Cmdline
    /// mode, for the host to render; `None` in any other mode.
    pub fn cmdline(&self) -> Option<String> {
        match &self.mode {
            Mode::Cmdline { prefix, buffer } => Some(format!("{prefix}{buffer}")),
            _ => None,
        }
    }

    /// A short display of any partially-typed mapping (e.g. `␣` after pressing
    /// the leader), for a status-line indicator. Empty when nothing is pending.
    pub fn pending_display(&self) -> String {
        self.map_pending
            .iter()
            .map(|k| match k {
                Key::Char(' ') => '␣',
                Key::Char(c) => *c,
                _ => '·',
            })
            .collect()
    }

    // --- undo/redo plumbing ---

    fn commit_undo(&mut self, cursor: Position) {
        let text = self.editor.cur_buffer().text.clone();
        self.editor.cur_buffer_mut().changedtick += 1;
        self.editor.cur_buffer_mut().undo.commit(&text, cursor);
    }

    /// Commit an undo checkpoint at the current cursor. Used by the core after
    /// running scripting (`:lua`/vimscript) that may have edited the buffer.
    pub fn checkpoint_undo(&mut self) {
        let cursor = self.editor.cursor();
        self.commit_undo(cursor);
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

    /// Put the cursor at `line`/`col`, clamped into the buffer — for the host
    /// jumping to a position it got from outside the editor (a quickfix entry,
    /// a tag, an LSP location).
    pub fn set_cursor_clamped(&mut self, line: usize, col: usize) {
        let buf = &self.editor.cur_buffer().text;
        let line = line.min(buf.line_count().saturating_sub(1));
        let pos = motion::clamp_normal(buf, Position::new(line, col));
        self.editor.set_cursor(pos);
    }

    pub fn lines(&self) -> Vec<String> {
        self.editor.cur_buffer().text.lines()
    }

    /// The active visual selection, normalized so `start <= end`, or `None`
    /// outside Visual mode. The frontend highlights this so the user can see
    /// what they're selecting.
    pub fn selection(&self) -> Option<Selection> {
        match self.mode {
            Mode::Visual { anchor, kind } => {
                let cursor = self.editor.cursor();
                let (start, end) = if anchor <= cursor { (anchor, cursor) } else { (cursor, anchor) };
                Some(Selection { kind, start, end })
            }
            _ => None,
        }
    }
}

// --- Ex command parsing helpers (free functions) ---------------------------

/// Split the command word from its argument. Handles the "glued" commands whose
/// argument follows with no separating space: `s///`, `g//`, `v//`, `>` `<` `!`
/// `=` `&`, and `:normal`/`:norm` (whose keys may be alphabetic).
fn split_ex(rest: &str) -> (String, String) {
    // `:normal`/`:norm {keys}` — keys aren't trimmed and may be alphabetic.
    for kw in ["normal", "norm"] {
        if let Some(after) = rest.strip_prefix(kw) {
            if after.is_empty() || after.starts_with([' ', '!']) {
                let after = after.strip_prefix('!').unwrap_or(after);
                let keys = after.strip_prefix(' ').unwrap_or(after);
                return ("normal".to_string(), keys.to_string());
            }
        }
    }
    // Single-char glued commands.
    for p in [">", "<", "!", "=", "&"] {
        if let Some(a) = rest.strip_prefix(p) {
            return (p.to_string(), a.trim().to_string());
        }
    }
    // Command word = leading alphabetic run.
    let end = rest.find(|c: char| !c.is_ascii_alphabetic()).unwrap_or(rest.len());
    let name = &rest[..end];
    let after = &rest[end..];
    match name {
        "s" | "su" | "sub" | "substitute" | "g" | "global" | "v" | "vglobal" => {
            (name.to_string(), after.to_string())
        }
        _ => (name.to_string(), after.trim().to_string()),
    }
}

/// Split a `:s` argument (`/pat/rep/flags`) on its delimiter, honoring
/// backslash-escaped delimiters. Returns `None` when there's no delimiter (a
/// bare `:s` that should repeat the last substitution).
fn split_subst(args: &str) -> Option<(String, String, String)> {
    let mut chars = args.chars();
    let delim = chars.next()?;
    if delim.is_alphanumeric() || delim.is_whitespace() || delim == '\\' || delim == '"' {
        return None;
    }
    let rest: Vec<char> = chars.collect();
    let mut fields: Vec<String> = vec![String::new()];
    let mut i = 0;
    while i < rest.len() {
        let c = rest[i];
        if c == '\\' && i + 1 < rest.len() {
            if rest[i + 1] == delim {
                fields.last_mut().unwrap().push(delim); // unescape \delim
            } else {
                fields.last_mut().unwrap().push('\\');
                fields.last_mut().unwrap().push(rest[i + 1]);
            }
            i += 2;
            continue;
        }
        if c == delim && fields.len() < 3 {
            fields.push(String::new());
        } else {
            fields.last_mut().unwrap().push(c);
        }
        i += 1;
    }
    Some((
        fields.first().cloned().unwrap_or_default(),
        fields.get(1).cloned().unwrap_or_default(),
        fields.get(2).cloned().unwrap_or_default(),
    ))
}

/// Split a `:g` argument (`/pat/cmd`) into pattern and command.
fn split_global(args: &str) -> Option<(String, String)> {
    let mut chars = args.chars();
    let delim = chars.next()?;
    if delim.is_alphanumeric() || delim.is_whitespace() {
        return None;
    }
    let rest: Vec<char> = chars.collect();
    let mut pat = String::new();
    let mut i = 0;
    while i < rest.len() {
        let c = rest[i];
        if c == '\\' && i + 1 < rest.len() {
            if rest[i + 1] == delim {
                pat.push(delim);
            } else {
                pat.push('\\');
                pat.push(rest[i + 1]);
            }
            i += 2;
            continue;
        }
        if c == delim {
            let cmd: String = rest[i + 1..].iter().collect();
            return Some((pat, cmd));
        }
        pat.push(c);
        i += 1;
    }
    Some((pat, String::new()))
}

/// Parse `:vimgrep /pattern/ [glob]` into its pattern and file glob. The
/// delimiter is whatever character opens the pattern (Vim allows any), and the
/// glob is optional (defaulting to the whole project).
fn parse_vimgrep(arg: &str) -> Option<(String, Option<String>)> {
    let arg = arg.trim();
    let mut chars = arg.chars();
    let delim = chars.next()?;
    if delim.is_alphanumeric() || delim.is_whitespace() {
        // Bare word form: `:vimgrep foo *.rs`.
        let (pattern, rest) = match arg.split_once(char::is_whitespace) {
            Some((p, r)) => (p.to_string(), r.trim()),
            None => (arg.to_string(), ""),
        };
        if pattern.is_empty() {
            return None;
        }
        return Some((pattern, (!rest.is_empty()).then(|| rest.to_string())));
    }
    let rest = &arg[delim.len_utf8()..];
    let end = rest.find(delim)?;
    let pattern = rest[..end].to_string();
    if pattern.is_empty() {
        return None;
    }
    let glob = rest[end + delim.len_utf8()..].trim();
    Some((pattern, (!glob.is_empty()).then(|| glob.to_string())))
}

impl Default for Session {
    fn default() -> Self {
        Session::new()
    }
}

/// The built-in normal-mode mappings. `<leader>` is Space; the chords expand to
/// real command lines so the behavior is identical to a user defining them in
/// config (`:nnoremap <Space>w :w<CR>`).
fn default_keymap() -> Keymap {
    let mut km = Keymap::default();
    km.set_normal("<Space>e", ":Files<CR>");
    km.set_normal("<Space>ff", ":Files<CR>");
    km.set_normal("<Space>w", ":w<CR>");
    km.set_normal("<Space>q", ":wq<CR>");
    km.set_normal("<Space>d", ":dash<CR>");
    km.set_normal("<Space>S", ":Find<CR>");
    // `<leader>1`..`<leader>9` jump to that tab/buffer (`:b N`).
    for n in 1..=9 {
        km.set_normal(&format!("<Space>{n}"), &format!(":b {n}<CR>"));
    }
    km
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
    fn visual_count_motion_extends_selection() {
        // `V3j` selects the current line plus the next three, then deletes them.
        let mut s = Session::with_text("a\nb\nc\nd\ne\nf");
        s.feed_str("V3jd");
        assert_eq!(s.lines(), vec!["e", "f"]);
    }

    #[test]
    fn visual_count_charwise() {
        // `v3l` extends the selection across four columns (inclusive).
        let mut s = Session::with_text("hello world");
        s.feed_str("v3ld");
        assert_eq!(s.lines(), vec!["o world"]);
    }

    #[test]
    fn visual_gg_extends_to_top() {
        // From the last line, `Vgg` selects up to the first line.
        let mut s = Session::with_text("a\nb\nc");
        s.feed_str("GVggd");
        assert_eq!(s.lines(), vec![""]);
    }

    #[test]
    fn selection_none_outside_visual() {
        let s = Session::with_text("hello");
        assert!(s.selection().is_none());
    }

    #[test]
    fn selection_normalizes_backward_drag() {
        use crate::mode::VisualKind;
        let mut s = Session::with_text("hello world");
        // Move right, enter visual, then drag left past the anchor.
        s.feed_str("llllvhh");
        let sel = s.selection().expect("in visual mode");
        assert_eq!(sel.kind, VisualKind::Char);
        // start must precede end regardless of drag direction.
        assert!(sel.start <= sel.end);
        assert_eq!(sel.start.col, 2);
        assert_eq!(sel.end.col, 4);
    }

    #[test]
    fn selection_clears_on_esc() {
        let mut s = Session::with_text("hello");
        s.feed_str("vl");
        assert!(s.selection().is_some());
        s.feed_str("<Esc>");
        assert!(s.selection().is_none());
    }

    #[test]
    fn ex_set_options() {
        let mut s = Session::with_text("x");
        assert!(!s.editor.global_options.number);
        s.feed_str(":set number ts=4 nowrap<CR>");
        assert!(s.editor.global_options.number);
        assert_eq!(s.editor.global_options.tabstop, 4);
        assert!(!s.editor.global_options.wrap);
        // Unknown option surfaces an E518 message.
        s.feed_str(":set bogus<CR>");
        assert!(s.take_effects().iter().any(|e| matches!(
            e,
            crate::ex::ExEffect::Message(m) if m.contains("E518")
        )));
    }

    #[test]
    fn ex_undo_redo() {
        let mut s = Session::with_text("hello");
        s.feed_str("x"); // delete 'h' -> "ello"
        assert_eq!(s.lines(), vec!["ello"]);
        s.feed_str(":undo<CR>");
        assert_eq!(s.lines(), vec!["hello"]);
        s.feed_str(":redo<CR>");
        assert_eq!(s.lines(), vec!["ello"]);
    }

    #[test]
    fn search_forward_and_repeat() {
        let mut s = Session::with_text("foo\nbar\nfoo\nbaz");
        s.feed_str("/foo<CR>"); // from line 0 → next "foo" is line 2
        assert_eq!(s.cursor().line, 2);
        s.feed_str("n"); // wraps to line 0
        assert_eq!(s.cursor().line, 0);
        s.feed_str("N"); // reverse → line 2
        assert_eq!(s.cursor().line, 2);
        assert!(s.search_highlight);
        s.feed_str(":noh<CR>");
        assert!(!s.search_highlight);
    }

    #[test]
    fn substitute_line_and_global_flag() {
        let mut s = Session::with_text("a a a\nb b b");
        s.feed_str(":s/a/X/<CR>"); // current line, first only
        assert_eq!(s.lines()[0], "X a a");
        s.feed_str(":%s/b/Y/g<CR>"); // whole file, all
        assert_eq!(s.lines()[1], "Y Y Y");
    }

    #[test]
    fn substitute_with_range_and_groups() {
        let mut s = Session::with_text("one\ntwo\nthree\nfour");
        // Swap around a captured group across a range.
        s.feed_str(":2,3s/\\(.*\\)/<\\1>/<CR>");
        assert_eq!(s.lines(), vec!["one", "<two>", "<three>", "four"]);
    }

    #[test]
    fn global_delete_and_substitute() {
        let mut s = Session::with_text("keep\ndrop x\nkeep\ndrop y");
        s.feed_str(":g/drop/d<CR>");
        assert_eq!(s.lines(), vec!["keep", "keep"]);

        let mut s2 = Session::with_text("a1\nb\na2\nc");
        s2.feed_str(":g/a/s/$/!/<CR>"); // append ! to lines containing 'a'
        assert_eq!(s2.lines(), vec!["a1!", "b", "a2!", "c"]);
    }

    #[test]
    fn vglobal_inverts() {
        let mut s = Session::with_text("a\nb\na\nc");
        s.feed_str(":v/a/d<CR>"); // delete lines NOT containing 'a'
        assert_eq!(s.lines(), vec!["a", "a"]);
    }

    #[test]
    fn range_delete_yank_put() {
        let mut s = Session::with_text("1\n2\n3\n4\n5");
        s.feed_str(":2,3d<CR>");
        assert_eq!(s.lines(), vec!["1", "4", "5"]);
        s.feed_str(":$put<CR>"); // put the deleted "2\n3" after the last line
        assert_eq!(s.lines(), vec!["1", "4", "5", "2", "3"]);
    }

    #[test]
    fn range_move_and_copy() {
        let mut s = Session::with_text("a\nb\nc\nd");
        s.feed_str(":1m$<CR>"); // move line 1 to the end
        assert_eq!(s.lines(), vec!["b", "c", "d", "a"]);
        let mut s2 = Session::with_text("a\nb\nc");
        s2.feed_str(":1t$<CR>"); // copy line 1 to the end
        assert_eq!(s2.lines(), vec!["a", "b", "c", "a"]);
    }

    #[test]
    fn range_join_shift_sort() {
        let mut s = Session::with_text("a\n  b\nc");
        s.feed_str(":1,2j<CR>");
        assert_eq!(s.lines(), vec!["a b", "c"]);

        let mut sh = Session::with_text("x\ny");
        sh.feed_str(":set sw=2<CR>:1,2><CR>");
        assert_eq!(sh.lines(), vec!["  x", "  y"]);

        let mut so = Session::with_text("banana\napple\ncherry");
        so.feed_str(":sort<CR>");
        assert_eq!(so.lines(), vec!["apple", "banana", "cherry"]);
        so.feed_str(":sort!<CR>");
        assert_eq!(so.lines(), vec!["cherry", "banana", "apple"]);
    }

    #[test]
    fn range_normal() {
        let mut s = Session::with_text("a\nb\nc");
        s.feed_str(":%normal IX<CR>"); // prepend X on every line
        assert_eq!(s.lines(), vec!["Xa", "Xb", "Xc"]);
    }

    #[test]
    fn visual_colon_seeds_range() {
        let mut s = Session::with_text("a\nb\nc\nd");
        s.feed_str("Vj:"); // select lines 1-2, then `:`
        // The command line is pre-seeded with the visual range.
        assert_eq!(s.cmdline().as_deref(), Some(":'<,'>"));
        s.feed_str("d<CR>");
        assert_eq!(s.lines(), vec!["c", "d"]);
    }

    #[test]
    fn gt_switches_tabs() {
        use crate::ex::{BufferCmd, ExEffect};
        let mut s = Session::with_text("x");
        s.feed_str("gt");
        assert!(s.take_effects().contains(&ExEffect::Buffer(BufferCmd::Next)));
        s.feed_str("gT");
        assert!(s.take_effects().contains(&ExEffect::Buffer(BufferCmd::Prev)));
        s.feed_str("2gt"); // count → absolute tab
        assert!(s.take_effects().contains(&ExEffect::Buffer(BufferCmd::Goto(2))));
    }

    /// Fill the list the way the host does after walking files.
    fn seeded_quickfix() -> Session {
        use crate::quickfix::{QfItem, QfKind};
        let mut s = Session::with_text("x");
        let items = (0..3)
            .map(|i| QfItem {
                path: format!("src/f{i}.rs").into(),
                line: i * 10,
                col: i,
                text: format!("hit {i}"),
                kind: QfKind::Match,
            })
            .collect();
        s.set_quickfix(items, ":vimgrep /hit/");
        s
    }

    #[test]
    fn quickfix_navigation_emits_jumps() {
        let mut s = seeded_quickfix();
        s.feed_str(":cnext<CR>");
        assert!(s.take_effects().contains(&ExEffect::Quickfix(QuickfixCmd::Jump {
            path: "src/f1.rs".into(),
            line: 10,
            col: 1,
        })));
        // `:cc N` is 1-based on the command line.
        s.feed_str(":cc 3<CR>");
        assert!(s.take_effects().contains(&ExEffect::Quickfix(QuickfixCmd::Jump {
            path: "src/f2.rs".into(),
            line: 20,
            col: 2,
        })));
        s.feed_str(":cfirst<CR>");
        assert!(s.take_effects().contains(&ExEffect::Quickfix(QuickfixCmd::Jump {
            path: "src/f0.rs".into(),
            line: 0,
            col: 0,
        })));
    }

    #[test]
    fn quickfix_navigation_on_an_empty_list_reports_it() {
        let mut s = Session::with_text("x");
        s.feed_str(":cnext<CR>");
        assert!(s.take_effects().contains(&ExEffect::Message("E42: no errors".into())));
    }

    #[test]
    fn quickfix_open_and_close_are_host_effects() {
        let mut s = Session::with_text("x");
        s.feed_str(":copen<CR>");
        assert!(s.take_effects().contains(&ExEffect::Quickfix(QuickfixCmd::Open)));
        s.feed_str(":cclose<CR>");
        assert!(s.take_effects().contains(&ExEffect::Quickfix(QuickfixCmd::Close)));
    }

    #[test]
    fn vimgrep_asks_the_host_to_walk_files() {
        let mut s = Session::with_text("x");
        s.feed_str(":vimgrep /fn main/ **/*.rs<CR>");
        assert!(s.take_effects().contains(&ExEffect::Quickfix(QuickfixCmd::Grep {
            pattern: "fn main".into(),
            glob: Some("**/*.rs".into()),
        })));
        // Without a glob the whole project is searched.
        s.feed_str(":vimgrep /todo/<CR>");
        assert!(s.take_effects().contains(&ExEffect::Quickfix(QuickfixCmd::Grep {
            pattern: "todo".into(),
            glob: None,
        })));
    }

    #[test]
    fn make_and_grep_ask_the_host_to_run_a_program() {
        let mut s = Session::with_text("x");
        s.feed_str(":make<CR>");
        let effects = s.take_effects();
        assert!(effects.iter().any(|e| matches!(
            e,
            ExEffect::Quickfix(QuickfixCmd::Run { program, .. }) if program == "cargo"
        )));
        s.feed_str(":grep todo<CR>");
        let effects = s.take_effects();
        assert!(effects.iter().any(|e| matches!(
            e,
            ExEffect::Quickfix(QuickfixCmd::Run { program, args, .. })
                if program == "grep" && args == &["-rn".to_string(), "todo".to_string()]
        )));
    }

    #[test]
    fn bang_command_asks_the_host_to_run_a_shell_command() {
        let mut s = Session::with_text("x");
        s.feed_str(":!echo hi<CR>");
        assert!(s
            .take_effects()
            .contains(&ExEffect::Shell("echo hi".to_string())));
    }

    #[test]
    fn bare_bang_with_no_command_reports_an_error() {
        let mut s = Session::with_text("x");
        s.feed_str(":!<CR>");
        assert!(s
            .take_effects()
            .contains(&ExEffect::Message("E34: No previous command".into())));
    }

    #[test]
    fn ranged_bang_is_rejected_rather_than_silently_ignoring_the_range() {
        let mut s = Session::with_text("a\nb\nc");
        s.feed_str(":%!sort<CR>");
        let effects = s.take_effects();
        assert!(!effects.contains(&ExEffect::Shell("sort".to_string())));
        assert!(effects.iter().any(|e| matches!(e, ExEffect::Message(m) if m.starts_with("E492"))));
    }

    #[test]
    fn parses_vimgrep_argument_forms() {
        assert_eq!(
            parse_vimgrep("/fn \\<main\\>/ src/**"),
            Some(("fn \\<main\\>".into(), Some("src/**".into())))
        );
        // Any delimiter, as in Vim.
        assert_eq!(parse_vimgrep("#a/b#"), Some(("a/b".into(), None)));
        // Bare word form.
        assert_eq!(parse_vimgrep("todo *.rs"), Some(("todo".into(), Some("*.rs".into()))));
        // Unterminated or empty patterns are rejected, not guessed at.
        assert_eq!(parse_vimgrep("/unclosed"), None);
        assert_eq!(parse_vimgrep("//"), None);
        assert_eq!(parse_vimgrep(""), None);
    }

    /// Three paragraphs separated by single empty lines.
    fn paragraphs() -> Session {
        //  0:one 1:two | 2:blank | 3:three 4:four | 5:blank | 6:five
        Session::with_text("one\ntwo\n\nthree\nfour\n\nfive")
    }

    #[test]
    fn brace_moves_between_paragraphs() {
        let mut s = paragraphs();
        s.feed_str("}");
        assert_eq!(s.cursor().line, 2, "to the blank line after the paragraph");
        s.feed_str("}");
        assert_eq!(s.cursor().line, 5);
        s.feed_str("}");
        assert_eq!(s.cursor().line, 6, "no boundary left, so the end of the buffer");
        s.feed_str("{");
        assert_eq!(s.cursor().line, 5);
        s.feed_str("{");
        assert_eq!(s.cursor().line, 2);
        s.feed_str("{");
        assert_eq!(s.cursor().line, 0, "and back to the start");
    }

    #[test]
    fn a_count_moves_that_many_paragraphs() {
        let mut s = paragraphs();
        s.feed_str("2}");
        assert_eq!(s.cursor().line, 5);
        s.feed_str("2{");
        assert_eq!(s.cursor().line, 0);
        // More than there are stops at the edge rather than refusing.
        s.feed_str("9}");
        assert_eq!(s.cursor().line, 6);
    }

    #[test]
    fn consecutive_blank_lines_are_one_boundary() {
        // Vim skips a run of blanks rather than stopping on each.
        let mut s = Session::with_text("one\n\n\n\ntwo\n\nthree");
        s.feed_str("}");
        assert_eq!(s.cursor().line, 1, "the first blank of the run");
        s.feed_str("}");
        assert_eq!(s.cursor().line, 5, "skipped the rest of the run and `two`");
    }

    #[test]
    fn a_whitespace_only_line_is_not_a_paragraph_boundary() {
        let mut s = Session::with_text("one\n   \ntwo\n\nthree");
        s.feed_str("}");
        assert_eq!(s.cursor().line, 3, "the spaces-only line does not count");
    }

    #[test]
    fn d_brace_deletes_to_the_paragraph_boundary() {
        let mut s = paragraphs();
        s.feed_str("d}");
        // Exclusive: the blank boundary line survives.
        assert_eq!(s.lines(), vec!["", "three", "four", "", "five"]);
    }

    #[test]
    fn d_brace_on_the_last_paragraph_deletes_through_the_end() {
        let mut s = paragraphs();
        s.feed_str("G"); // last line, `five`
        s.feed_str("d}");
        assert_eq!(s.lines(), vec!["one", "two", "", "three", "four", "", ""]);
    }

    #[test]
    fn d_brace_from_mid_line_keeps_the_boundary_line() {
        // `:help exclusive` rule 1: the end moves back to the end of the
        // previous line, so the blank line is left behind rather than joined.
        let mut s = Session::with_text("one\ntwo\n\nthree");
        s.feed_str("ld}");
        assert_eq!(s.lines(), vec!["o", "", "three"]);
    }

    #[test]
    fn dw_on_a_lines_last_word_does_not_join_the_next_line() {
        // The classic exclusive-motion case: without `:help exclusive` this
        // deletes the newline too and pulls `gamma` up.
        let mut s = Session::with_text("alpha beta\ngamma");
        s.feed_str("$dw");
        assert_eq!(s.lines(), vec!["alpha bet", "gamma"]);
        // From the start of the last word, the whole word goes but the line
        // break survives.
        let mut s = Session::with_text("alpha beta\ngamma");
        s.feed_str("wdw");
        assert_eq!(s.lines(), vec!["alpha ", "gamma"]);
    }

    #[test]
    fn visual_brace_extends_the_selection() {
        let mut s = paragraphs();
        s.feed_str("v}");
        let sel = s.selection().expect("visual selection");
        assert_eq!(sel.start.line, 0);
        assert_eq!(sel.end.line, 2, "extends to the paragraph boundary");
    }

    #[test]
    fn brace_motions_on_a_single_line_buffer_do_not_move() {
        let mut s = Session::with_text("only");
        s.feed_str("}");
        assert_eq!(s.cursor().line, 0);
        s.feed_str("{");
        assert_eq!(s.cursor().line, 0);
    }

    /// A session with a loaded tags table and a buffer that mentions them.
    fn tagged() -> Session {
        let mut s = Session::with_text("use editor::Editor;\nfn call() { helper(); }\n");
        s.set_tags(crate::tags::TagTable::parse(
            "Editor\tsrc/editor.rs\t/^pub struct Editor {$/;\"\ts\n\
             helper\tsrc/a.rs\t10;\"\tf\n\
             helper\tsrc/b.rs\t20;\"\tf\n",
        ));
        s
    }

    #[test]
    fn ctrl_bracket_looks_up_the_word_under_the_cursor() {
        let mut s = tagged();
        s.feed_str("fE"); // onto `Editor`
        s.feed(Key::Ctrl(']'));
        assert!(s.take_effects().contains(&ExEffect::Tag(TagCmd::Lookup {
            name: "Editor".into()
        })));
    }

    #[test]
    fn the_word_under_the_cursor_extends_both_ways() {
        let mut s = tagged();
        s.feed_str("fd"); // middle of `editor`
        assert_eq!(s.word_at_cursor().as_deref(), Some("editor"));
        s.feed_str("0");
        assert_eq!(s.word_at_cursor().as_deref(), Some("use"));
        // On punctuation there is no identifier.
        s.feed_str("$");
        assert_eq!(s.word_at_cursor(), None);
    }

    #[test]
    fn a_lookup_with_no_identifier_reports_it() {
        let mut s = Session::with_text("   ");
        s.feed(Key::Ctrl(']'));
        assert!(s
            .take_effects()
            .iter()
            .any(|e| matches!(e, ExEffect::Message(m) if m.contains("E349"))));
    }

    #[test]
    fn select_tag_pushes_the_stack_and_returns_the_first_match() {
        let mut s = tagged();
        s.feed_str("j"); // line 1
        let tag = s.select_tag("helper", "src/main.rs").expect("helper is in the table");
        assert_eq!(tag.path, "src/a.rs");
        assert_eq!(s.tag_match_count(), 2, "both definitions are remembered");
        let top = &s.tagstack().entries()[0];
        assert_eq!((top.name.as_str(), top.path.as_str(), top.line), ("helper", "src/main.rs", 1));
        // An unknown name selects nothing and leaves the stack alone.
        assert!(s.select_tag("nope", "src/main.rs").is_none());
        assert_eq!(s.tagstack().entries().len(), 1);
    }

    #[test]
    fn ctrl_t_returns_to_the_pushed_position() {
        let mut s = tagged();
        s.feed_str("j");
        s.select_tag("helper", "src/main.rs");
        s.take_effects();
        s.feed(Key::Ctrl('t'));
        assert!(s.take_effects().contains(&ExEffect::Tag(TagCmd::Return {
            path: "src/main.rs".into(),
            line: 1,
            col: 0,
        })));
        // Popping an empty stack reports rather than jumping somewhere random.
        s.feed(Key::Ctrl('t'));
        assert!(s
            .take_effects()
            .iter()
            .any(|e| matches!(e, ExEffect::Message(m) if m.contains("E73"))));
    }

    #[test]
    fn tnext_walks_the_definitions_of_an_overloaded_name() {
        let mut s = tagged();
        s.select_tag("helper", "src/main.rs");
        s.take_effects();
        s.feed_str(":tnext<CR>");
        let effects = s.take_effects();
        assert!(effects.iter().any(|e| matches!(
            e,
            ExEffect::Tag(TagCmd::Jump { path, .. }) if path == "src/b.rs"
        )));
        assert!(effects
            .iter()
            .any(|e| matches!(e, ExEffect::Message(m) if m == "tag 2 of 2")));
        // Past the end it reports instead of wrapping.
        s.feed_str(":tnext<CR>");
        assert!(s
            .take_effects()
            .iter()
            .any(|e| matches!(e, ExEffect::Message(m) if m.contains("E425"))));
    }

    #[test]
    fn ex_tag_takes_an_explicit_name() {
        let mut s = tagged();
        s.feed_str(":tag Editor<CR>");
        assert!(s.take_effects().contains(&ExEffect::Tag(TagCmd::Lookup {
            name: "Editor".into()
        })));
    }

    #[test]
    fn ex_tags_lists_the_stack() {
        let mut s = tagged();
        s.feed_str(":tags<CR>");
        assert!(s
            .take_effects()
            .iter()
            .any(|e| matches!(e, ExEffect::Message(m) if m.contains("empty"))));
        s.select_tag("helper", "src/main.rs");
        s.take_effects();
        s.feed_str(":tags<CR>");
        assert!(s
            .take_effects()
            .iter()
            .any(|e| matches!(e, ExEffect::Message(m) if m.contains("helper") && m.contains("src/main.rs"))));
    }

    /// Ten lines, `0`..`9`, cursor at the top.
    fn numbered() -> Session {
        Session::with_text("0\n1\n2\n3\n4\n5\n6\n7\n8\n9")
    }

    #[test]
    fn zf_folds_the_lines_a_motion_sweeps() {
        let mut s = numbered();
        s.feed_str("jj"); // line 2
        s.feed_str("zf3j"); // fold lines 2..=5
        let folds = s.folds().all();
        assert_eq!(folds.len(), 1);
        assert_eq!((folds[0].start, folds[0].end), (2, 5));
        assert!(folds[0].closed, "a new fold starts closed, as in Vim");
        // The text is untouched — `zf` is not an edit.
        assert_eq!(s.lines().len(), 10);
    }

    #[test]
    fn j_and_k_step_over_a_closed_fold() {
        let mut s = numbered();
        s.feed_str("jjzf3j"); // fold 2..=5, cursor on its first line
        assert_eq!(s.cursor().line, 2);
        s.feed_str("j");
        assert_eq!(s.cursor().line, 6, "one press clears the whole fold");
        s.feed_str("k");
        assert_eq!(s.cursor().line, 2, "and back onto its head, not inside it");
        // A count still counts *visible* lines.
        s.feed_str("2j");
        assert_eq!(s.cursor().line, 7);
    }

    #[test]
    fn opening_a_fold_restores_normal_movement() {
        let mut s = numbered();
        s.feed_str("jjzf3j");
        s.feed_str("zo");
        s.feed_str("j");
        assert_eq!(s.cursor().line, 3, "inside the open fold");
        s.feed_str("zc"); // close it again from inside
        assert_eq!(s.cursor().line, 2, "the cursor snaps to the fold's head");
    }

    #[test]
    fn za_toggles_and_zr_zm_act_on_everything() {
        let mut s = numbered();
        s.feed_str("jjzf3j");
        s.feed_str("za");
        assert!(!s.folds().all()[0].closed);
        s.feed_str("za");
        assert!(s.folds().all()[0].closed);
        s.feed_str("zR");
        assert!(s.folds().all().iter().all(|f| !f.closed));
        s.feed_str("zM");
        assert!(s.folds().all().iter().all(|f| f.closed));
    }

    #[test]
    fn zd_deletes_a_fold_and_ze_clears_them_all() {
        let mut s = numbered();
        s.feed_str("zf2j"); // 0..=2
        s.feed_str("Gzfk"); // another near the end
        assert_eq!(s.folds().all().len(), 2);
        s.feed_str("gg");
        s.feed_str("zd");
        assert_eq!(s.folds().all().len(), 1);
        s.feed_str("zE");
        assert!(s.folds().is_empty());
    }

    #[test]
    fn zi_disables_folding_without_losing_folds() {
        let mut s = numbered();
        s.feed_str("jjzf3j");
        s.feed_str("zi");
        assert!(!s.folds().enabled);
        s.feed_str("j");
        assert_eq!(s.cursor().line, 3, "movement ignores folds while disabled");
        s.feed_str("zi");
        assert!(s.folds().enabled);
        assert_eq!(s.folds().all().len(), 1, "the fold survived");
    }

    #[test]
    fn ex_fold_commands_take_a_range() {
        let mut s = numbered();
        s.feed_str(":2,5fold<CR>");
        let folds = s.folds().all();
        assert_eq!((folds[0].start, folds[0].end), (1, 4), "1-based range, 0-based lines");
        s.feed_str(":2foldopen<CR>");
        assert!(!s.folds().all()[0].closed);
        s.feed_str(":2foldclose<CR>");
        assert!(s.folds().all()[0].closed);
    }

    #[test]
    fn set_foldmethod_indent_derives_folds() {
        let mut s = Session::with_text("fn a() {\n    one;\n    two;\n}\nfn b() {}");
        assert!(s.folds().is_empty());
        s.feed_str(":set foldmethod=indent<CR>");
        s.feed_str(":set shiftwidth=4<CR>");
        let ranges: Vec<(usize, usize)> = s.folds().all().iter().map(|f| (f.start, f.end)).collect();
        assert!(ranges.contains(&(0, 2)), "the indented body folds: {ranges:?}");
        // An unknown fold method is reported rather than silently ignored.
        s.feed_str(":set foldmethod=nonsense<CR>");
        assert!(s
            .take_effects()
            .iter()
            .any(|e| matches!(e, ExEffect::Message(m) if m.contains("E518"))));
    }

    #[test]
    fn zj_and_zk_move_between_folds() {
        let mut s = numbered();
        s.feed_str("jjzf2j"); // fold 2..=4
        s.feed_str("zR"); // open it, so the boundaries are reachable
        s.feed_str("G"); // last line
        s.feed_str("zk");
        assert_eq!(s.cursor().line, 4, "to the end of the previous fold");
        s.feed_str("gg");
        s.feed_str("zj");
        assert_eq!(s.cursor().line, 2, "to the start of the next fold");
        // With the fold closed, landing "inside" it puts the cursor on its
        // head — a closed fold counts as one line.
        s.feed_str("zM");
        s.feed_str("G");
        s.feed_str("zk");
        assert_eq!(s.cursor().line, 2);
    }

    #[test]
    fn leader_d_opens_dashboard() {
        use crate::ex::ExEffect;
        let mut s = Session::with_text("x");
        s.feed_str(" d"); // <leader>d
        assert!(s.take_effects().contains(&ExEffect::OpenDashboard));
    }

    #[test]
    fn leader_s_opens_find_and_replace_seeded_from_the_cursor() {
        use crate::ex::ExEffect;
        let mut s = Session::with_text("let widget = 1;");
        s.feed_str("wl"); // onto `widget`
        s.feed_str(" S"); // <leader>S
        assert!(s
            .take_effects()
            .contains(&ExEffect::OpenReplace { pattern: Some("widget".into()) }));
    }

    #[test]
    fn find_takes_an_explicit_pattern_over_the_cursor_word() {
        use crate::ex::ExEffect;
        let mut s = Session::with_text("let widget = 1;");
        s.feed_str(r":Find \<fn\><CR>");
        assert!(s
            .take_effects()
            .contains(&ExEffect::OpenReplace { pattern: Some(r"\<fn\>".into()) }));
        // And off a word entirely, a bare `:Find` seeds nothing rather than
        // seeding whatever punctuation is under the cursor.
        let mut s = Session::with_text("   ");
        s.feed_str(":Find<CR>");
        assert!(s.take_effects().contains(&ExEffect::OpenReplace { pattern: None }));
    }

    #[test]
    fn ex_map_defines_normal_mapping() {
        let mut s = Session::with_text("a\nb\nc");
        s.feed_str(":nnoremap Q dd<CR>"); // map Q to delete-line
        s.feed_str("Q");
        assert_eq!(s.lines(), vec!["b", "c"]);
    }

    #[test]
    fn ex_command_defines_user_command() {
        let mut s = Session::with_text("1\n2\n3\n4");
        s.feed_str(":command Chop 2,3d<CR>"); // :Chop deletes lines 2-3
        s.feed_str(":Chop<CR>");
        assert_eq!(s.lines(), vec!["1", "4"]);
    }

    #[test]
    fn ex_scripting_emits_effects() {
        // The engine can't run Lua/Vimscript itself (the interpreters live in the
        // core), so it queues them as effects for the host to execute.
        let mut s = Session::with_text("x");
        s.feed_str(":let g:n = 1<CR>");
        assert!(s
            .take_effects()
            .iter()
            .any(|e| matches!(e, crate::ex::ExEffect::Vimscript(src) if src.contains("let"))));
        s.feed_str(":lua print(1)<CR>");
        assert!(s
            .take_effects()
            .iter()
            .any(|e| matches!(e, crate::ex::ExEffect::Lua(_))));
    }

    #[test]
    fn cmdline_goto_line() {
        let mut s = Session::with_text("l1\nl2\nl3\nl4");
        s.feed_str(":3<CR>");
        assert_eq!(s.cursor().line, 2);
    }

    #[test]
    fn ex_write_quit_effects_and_modified_guard() {
        use crate::ex::ExEffect;
        let mut s = Session::with_text("hello");
        assert!(!s.is_modified());

        s.feed_str("x"); // an edit dirties the buffer
        assert!(s.is_modified());

        // `:q` on a modified buffer is refused with an error, not a quit.
        s.feed_str(":q<CR>");
        assert!(matches!(s.take_effects().as_slice(), [ExEffect::Message(_)]));

        // `:w` marks it saved and emits a Write effect.
        s.feed_str(":w<CR>");
        assert!(!s.is_modified());
        assert!(matches!(s.take_effects().as_slice(), [ExEffect::Write { force: false }]));

        // Now clean, `:q` quits.
        s.feed_str(":q<CR>");
        assert!(matches!(s.take_effects().as_slice(), [ExEffect::Quit { force: false }]));

        // `:q!` overrides a dirty buffer.
        s.feed_str("x");
        s.feed_str(":q!<CR>");
        assert!(matches!(s.take_effects().as_slice(), [ExEffect::Quit { force: true }]));
    }

    #[test]
    fn find_char_and_repeat() {
        let mut s = Session::with_text("foo.bar.baz");
        s.feed_str("f."); // jump to first dot
        assert_eq!(s.cursor().col, 3);
        s.feed_str(";"); // next dot
        assert_eq!(s.cursor().col, 7);
        s.feed_str(","); // back to first dot
        assert_eq!(s.cursor().col, 3);
    }

    #[test]
    fn delete_to_find_char_inclusive() {
        let mut s = Session::with_text("foo(bar)baz");
        s.feed_str("df)"); // delete through the ')'
        assert_eq!(s.lines(), vec!["baz"]);
    }

    #[test]
    fn percent_matches_bracket() {
        let mut s = Session::with_text("a(bcd)e");
        s.feed_str("f("); // onto the '('
        s.feed_str("%"); // to the ')'
        assert_eq!(s.cursor().col, 5);
    }

    #[test]
    fn delete_inner_word_and_around() {
        let mut s = Session::with_text("foo bar baz");
        s.feed_str("w"); // onto "bar"
        s.feed_str("diw");
        assert_eq!(s.lines(), vec!["foo  baz"]);
        let mut s = Session::with_text("foo bar baz");
        s.feed_str("w");
        s.feed_str("daw"); // word + trailing space
        assert_eq!(s.lines(), vec!["foo baz"]);
    }

    #[test]
    fn change_inside_parens() {
        let mut s = Session::with_text("call(old)");
        s.feed_str("f(");
        s.feed_str("ci(new<Esc>");
        assert_eq!(s.lines(), vec!["call(new)"]);
    }

    #[test]
    fn replace_and_toggle_case() {
        let mut s = Session::with_text("cat");
        s.feed_str("rb"); // replace 'c' -> 'b'
        assert_eq!(s.lines(), vec!["bat"]);
        let mut s = Session::with_text("abc");
        s.feed_str("3~"); // toggle case of 3 chars
        assert_eq!(s.lines(), vec!["ABC"]);
    }

    #[test]
    fn case_operator_and_indent() {
        let mut s = Session::with_text("hello world");
        s.feed_str("gUw"); // uppercase the word
        assert_eq!(s.lines(), vec!["HELLO world"]);
        let mut s = Session::with_text("x");
        s.feed_str(">>"); // indent the line
        assert_eq!(s.lines(), vec!["    x"]);
        s.feed_str("<<"); // dedent
        assert_eq!(s.lines(), vec!["x"]);
    }

    #[test]
    fn dot_repeats_last_change() {
        let mut s = Session::with_text("aaaa");
        s.feed_str("x"); // delete one 'a'
        assert_eq!(s.lines(), vec!["aaa"]);
        s.feed_str("."); // repeat
        s.feed_str("."); // repeat
        assert_eq!(s.lines(), vec!["a"]);
    }

    #[test]
    fn dot_repeats_insert_change() {
        let mut s = Session::with_text("one\ntwo");
        s.feed_str("ciwX<Esc>"); // change inner word -> "X"
        assert_eq!(s.lines(), vec!["X", "two"]);
        s.feed_str("j0"); // to line 2 start
        s.feed_str("."); // repeat the ciw
        assert_eq!(s.lines(), vec!["X", "X"]);
    }

    #[test]
    fn leader_space_expands_to_command() {
        use crate::ex::ExEffect;
        let mut s = Session::with_text("hello");
        s.feed_str("x"); // dirty it so <Space>w has something to save
        s.feed_str(" w"); // <leader>w -> :w<CR>
        assert!(!s.is_modified());
        assert!(matches!(s.take_effects().as_slice(), [ExEffect::Write { .. }]));
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
