//! The central [`Editor`] context — the replacement for Neovim's global
//! `curbuf`/`curwin`/`curtab` triad and the ~590 globals around them.
//!
//! Every operation that C implicitly performs on "the current buffer" becomes a
//! method on `Editor` taking explicit handles. Buffers and windows live in
//! arenas indexed by `Copy` handles, so a stale handle is a clean `None` rather
//! than a dangling pointer.

use crate::quickfix::QuickfixList;
use crate::tags::{TagMatches, TagStack, TagTable};
use crate::window::{Frame, Window};
use ctrlvim_options::{BufferOptions, GlobalOptions, OptionContext};
use ctrlvim_text::{Buffer, MarkStore, Namespace, Registers, UndoTree};
use ctrlvim_types::{BufferId, Position, WindowId};
use std::collections::HashMap;

/// Decoration data for one extmark, alongside its position in
/// [`BufferState::marks`] (which only tracks the gravity-following position
/// itself — this is everything `nvim_buf_set_extmark`'s `opts` dict can carry
/// that a renderer needs: `vim.diagnostic`'s underlines and inline messages
/// are exactly this).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ExtmarkMeta {
    /// The end of the marked range, if it covers more than one position
    /// (`opts.end_row`/`opts.end_col` in real Neovim).
    pub end_line: Option<usize>,
    pub end_col: Option<usize>,
    /// Highlight group to apply across the marked range.
    pub hl_group: Option<String>,
    /// Virtual text chunks appended after the line (`opts.virt_text`): each
    /// is `(text, highlight_group)`.
    pub virt_text: Vec<(String, Option<String>)>,
}

/// Everything the editor knows about one buffer. Combines the text engine
/// pieces (`ctrlvim-text`) with editor-level metadata.
pub struct BufferState {
    pub text: Buffer,
    pub undo: UndoTree,
    pub marks: MarkStore,
    /// Decoration data for extmarks that carry more than a bare position —
    /// see [`ExtmarkMeta`]. Keyed the same way a mark itself is: namespace +
    /// id. An extmark with no entry here is a plain position-only mark.
    pub extmark_meta: HashMap<(Namespace, u32), ExtmarkMeta>,
    pub options: BufferOptions,
    pub name: Option<String>,
    /// Neovim's `b:changedtick`, bumped on every text change.
    pub changedtick: u64,
    /// The `changedtick` as of the last write; the buffer is "modified"
    /// (`'modified'`) whenever `changedtick` has moved past it.
    pub saved_changedtick: u64,
}

impl BufferState {
    fn new(text: Buffer, name: Option<String>) -> Self {
        let undo = UndoTree::new(&text, Position::default());
        BufferState {
            text,
            undo,
            marks: MarkStore::new(),
            extmark_meta: HashMap::new(),
            options: BufferOptions::default(),
            name,
            changedtick: 1,
            saved_changedtick: 1,
        }
    }

    /// Whether the buffer has unsaved changes (`'modified'`).
    pub fn modified(&self) -> bool {
        self.changedtick != self.saved_changedtick
    }

    /// Mark the current state as the on-disk state (after `:w`).
    pub fn mark_saved(&mut self) {
        self.saved_changedtick = self.changedtick;
    }
}

/// Where a floating window is positioned (`nvim_open_win`'s `relative`).
/// `Editor` (the default) anchors are absolute over the whole editor grid;
/// `Cursor` anchors are relative to the current window's cursor — what a
/// hover/signature-help popup wants, so it tracks the cursor across scrolls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FloatRelative {
    Editor,
    Cursor,
}

/// A floating window's placement — `nvim_open_win`'s `config` dict, the
/// subset that matters without a real compositor: no z-index stacking rules
/// beyond "opened later draws on top", no `relative = 'win'` (anchoring to
/// another *window* rather than the cursor or the whole editor).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FloatConfig {
    pub relative: FloatRelative,
    pub row: i64,
    pub col: i64,
    pub width: usize,
    pub height: usize,
    pub border: bool,
}

/// The editor context.
pub struct Editor {
    buffers: Vec<Option<BufferState>>,
    windows: Vec<Option<Window>>,
    current_window: WindowId,
    /// Split layout for the (single, for now) tabpage.
    pub layout: Frame,
    /// Floating windows, deliberately *not* part of [`Editor::layout`] — a
    /// float is drawn on top of the split layout, not a participant in it
    /// (closing the last split must still refuse; a float never counts).
    /// Ordered by open time, which doubles as z-order (later draws on top).
    floats: Vec<(WindowId, FloatConfig)>,
    pub registers: Registers,
    pub global_options: GlobalOptions,
    /// The quickfix list (`:make`, `:grep`, `:vimgrep`). One global list, as in
    /// Vim; per-window location lists are a later variant of the same type.
    pub quickfix: QuickfixList,
    /// The loaded tags file, filled by the host (`Ctrl-]`, `:tag`).
    pub tags: TagTable,
    /// Where `Ctrl-]` jumped from, for `Ctrl-T`.
    pub tagstack: TagStack,
    /// The definitions of the name most recently looked up, so `:tnext` /
    /// `:tprev` can walk an overloaded name.
    pub tag_matches: Option<TagMatches>,
}

impl Editor {
    /// Create an editor with a single empty buffer shown in a single window.
    pub fn new() -> Self {
        let buf = BufferState::new(Buffer::new(), None);
        let win = Window::new(BufferId(1));
        Editor {
            buffers: vec![Some(buf)],
            windows: vec![Some(win)],
            current_window: WindowId(1),
            layout: Frame::Leaf(WindowId(1)),
            floats: Vec::new(),
            registers: Registers::new(),
            global_options: GlobalOptions::default(),
            quickfix: QuickfixList::new(),
            tags: TagTable::new(),
            tagstack: TagStack::new(),
            tag_matches: None,
        }
    }

    /// Load `text` into the current buffer (as if opening a file).
    pub fn load_str(&mut self, text: &str, name: Option<&str>) {
        let bid = self.current_buffer_id();
        let buf = Buffer::from_str(text);
        let state = BufferState::new(buf, name.map(str::to_string));
        self.buffers[(bid.0 - 1) as usize] = Some(state);
        self.window_mut(self.current_window).unwrap().cursor = Position::default();
    }

    /// Create a new buffer, returning its id.
    pub fn create_buffer(&mut self, text: Buffer, name: Option<String>) -> BufferId {
        let id = BufferId(self.buffers.len() as u32 + 1);
        self.buffers.push(Some(BufferState::new(text, name)));
        id
    }

    /// Every live buffer id, in creation order (`nvim_list_bufs`).
    pub fn buffer_ids(&self) -> Vec<BufferId> {
        self.buffers
            .iter()
            .enumerate()
            .filter_map(|(i, b)| b.is_some().then(|| BufferId(i as u32 + 1)))
            .collect()
    }

    // --- handle resolution ---
    //
    // Handles are 1-based, matching real Neovim exactly: `0` is a reserved
    // sentinel a caller uses to mean "the current buffer/window" (real
    // `nvim_buf_*`/`nvim_win_*` resolve it that way; ctrlvim's own
    // `nvim_*` functions ask the caller to fetch the current handle
    // explicitly instead — see `ctrlvim-api/src/functions.rs`'s note on
    // this). What matters here is that `0` is *never* a real buffer or
    // window, because vendored Neovim runtime Lua (`vim.diagnostic`,
    // among others) asserts exactly that. Internally, `self.buffers`/
    // `self.windows` stay 0-indexed `Vec`s; handle `N` lives at index
    // `N - 1`.

    pub fn current_window_id(&self) -> WindowId {
        self.current_window
    }

    pub fn current_buffer_id(&self) -> BufferId {
        self.window(self.current_window).unwrap().buffer
    }

    pub fn window(&self, id: WindowId) -> Option<&Window> {
        let idx = id.0.checked_sub(1)?;
        self.windows.get(idx as usize).and_then(|w| w.as_ref())
    }

    pub fn window_mut(&mut self, id: WindowId) -> Option<&mut Window> {
        let idx = id.0.checked_sub(1)?;
        self.windows.get_mut(idx as usize).and_then(|w| w.as_mut())
    }

    pub fn buffer(&self, id: BufferId) -> Option<&BufferState> {
        let idx = id.0.checked_sub(1)?;
        self.buffers.get(idx as usize).and_then(|b| b.as_ref())
    }

    pub fn buffer_mut(&mut self, id: BufferId) -> Option<&mut BufferState> {
        let idx = id.0.checked_sub(1)?;
        self.buffers.get_mut(idx as usize).and_then(|b| b.as_mut())
    }

    // --- convenience accessors for the current window/buffer ---

    pub fn cur_window(&self) -> &Window {
        self.window(self.current_window).unwrap()
    }

    pub fn cur_window_mut(&mut self) -> &mut Window {
        let id = self.current_window;
        self.window_mut(id).unwrap()
    }

    pub fn cur_buffer(&self) -> &BufferState {
        self.buffer(self.current_buffer_id()).unwrap()
    }

    pub fn cur_buffer_mut(&mut self) -> &mut BufferState {
        let id = self.current_buffer_id();
        self.buffer_mut(id).unwrap()
    }

    /// The cursor of the current window.
    pub fn cursor(&self) -> Position {
        self.cur_window().cursor
    }

    /// Set the current window's cursor, clamping to a valid Normal-mode
    /// position within the current buffer.
    pub fn set_cursor(&mut self, pos: Position) {
        let clamped = crate::motion::clamp_normal(&self.cur_buffer().text, pos);
        self.cur_window_mut().cursor = clamped;
    }

    /// Resolve the effective options for the current window/buffer.
    pub fn options(&self) -> OptionContext<'_> {
        OptionContext {
            global: &self.global_options,
            buffer: &self.cur_buffer().options,
            window: &self.cur_window().options,
        }
    }

    /// Split the current window, updating the layout tree. `vertical` chooses a
    /// Row (side-by-side) vs Col (stacked) split. Returns the new window id.
    pub fn split_current(&mut self, vertical: bool) -> WindowId {
        self.split_current_placed(vertical, false)
    }

    /// Split the current window, placing the new one *before* the current one
    /// (above for a horizontal split, left for a vertical one) when `before` is
    /// set. That is Vim's default; `'splitbelow'`/`'splitright'` invert it.
    pub fn split_current_placed(&mut self, vertical: bool, before: bool) -> WindowId {
        let cur = self.current_window;
        let bufid = self.window(cur).unwrap().buffer;
        let new_id = WindowId(self.windows.len() as u32 + 1);
        self.windows.push(Some(Window::new(bufid)));
        // The new window inherits the current one's cursor, as Vim does — a
        // split shows the same view twice rather than jumping to the top.
        let cursor = self.window(cur).unwrap().cursor;
        if let Some(w) = self.window_mut(new_id) {
            w.cursor = cursor;
        }
        self.layout = Self::split_in_frame(
            std::mem::replace(&mut self.layout, Frame::Leaf(cur)),
            cur,
            new_id,
            vertical,
            before,
        );
        new_id
    }

    fn split_in_frame(
        frame: Frame,
        target: WindowId,
        new_id: WindowId,
        vertical: bool,
        before: bool,
    ) -> Frame {
        match frame {
            Frame::Leaf(id) if id == target => {
                let pair = if before {
                    vec![Frame::Leaf(new_id), Frame::Leaf(id)]
                } else {
                    vec![Frame::Leaf(id), Frame::Leaf(new_id)]
                };
                if vertical {
                    Frame::Row(pair)
                } else {
                    Frame::Col(pair)
                }
            }
            Frame::Leaf(id) => Frame::Leaf(id),
            Frame::Row(children) => Frame::Row(
                children
                    .into_iter()
                    .map(|c| Self::split_in_frame(c, target, new_id, vertical, before))
                    .collect(),
            ),
            Frame::Col(children) => Frame::Col(
                children
                    .into_iter()
                    .map(|c| Self::split_in_frame(c, target, new_id, vertical, before))
                    .collect(),
            ),
        }
    }

    /// Focus a different window.
    pub fn focus_window(&mut self, id: WindowId) -> bool {
        if self.window(id).is_some() {
            self.current_window = id;
            true
        } else {
            false
        }
    }

    /// All window ids in layout order, followed by any open floats
    /// (`nvim_list_wins` includes floats in the current tabpage).
    pub fn window_ids(&self) -> Vec<WindowId> {
        let mut ids = self.layout.windows();
        ids.extend(self.float_ids());
        ids
    }

    /// Open a floating window over `buffer` — `nvim_open_win` with a real
    /// buffer's worth of behavior (cursor/text access through the ordinary
    /// [`Editor::window`] accessors) but none of the split-layout machinery.
    pub fn open_float(&mut self, buffer: BufferId, config: FloatConfig) -> WindowId {
        let new_id = WindowId(self.windows.len() as u32 + 1);
        let mut win = Window::new(buffer);
        win.width = config.width;
        win.height = config.height;
        self.windows.push(Some(win));
        self.floats.push((new_id, config));
        new_id
    }

    /// Close a floating window. Returns `false` if `id` isn't an open float
    /// (including a *split* window — use [`Editor::close_window`] for those).
    pub fn close_float(&mut self, id: WindowId) -> bool {
        let before = self.floats.len();
        self.floats.retain(|(fid, _)| *fid != id);
        if self.floats.len() == before {
            return false;
        }
        if let Some(idx) = id.0.checked_sub(1).map(|i| i as usize).filter(|i| *i < self.windows.len()) {
            self.windows[idx] = None;
        }
        if self.current_window == id {
            self.current_window = self.layout.windows()[0];
        }
        true
    }

    /// Ids of every open float, in open order (which doubles as z-order).
    pub fn float_ids(&self) -> Vec<WindowId> {
        self.floats.iter().map(|(id, _)| *id).collect()
    }

    /// A float's placement config, if `id` is an open float.
    pub fn float_config(&self, id: WindowId) -> Option<&FloatConfig> {
        self.floats.iter().find(|(fid, _)| *fid == id).map(|(_, c)| c)
    }

    pub fn window_count(&self) -> usize {
        self.layout.windows().len()
    }

    /// Move focus to the next window in layout order (`<C-w>w`).
    pub fn focus_next(&mut self) {
        let wins = self.layout.windows();
        if wins.len() < 2 {
            return;
        }
        let idx = wins.iter().position(|w| *w == self.current_window).unwrap_or(0);
        self.current_window = wins[(idx + 1) % wins.len()];
    }

    /// Close a window (`<C-w>q`/`:close`). Refuses to close the last window.
    pub fn close_window(&mut self, id: WindowId) -> bool {
        if self.window_count() < 2 {
            return false;
        }
        match self.layout.without(id) {
            Some(new_layout) => {
                self.layout = new_layout;
                if let Some(idx) = id.0.checked_sub(1).map(|i| i as usize).filter(|i| *i < self.windows.len()) {
                    self.windows[idx] = None;
                }
                if self.current_window == id {
                    self.current_window = self.layout.windows()[0];
                }
                true
            }
            None => false,
        }
    }

    /// Compute the on-screen rectangle of every window given a total terminal
    /// size — the model a Ratatui frontend renders from (`ctrlvim_win_get_position`
    /// + dimensions, all at once).
    pub fn layout_rects(&self, width: usize, height: usize) -> Vec<(WindowId, usize, usize, usize, usize)> {
        self.layout.layout(0, 0, width, height)
    }

    /// Move focus to the nearest window in `dir` (`<C-w>h/j/k/l`).
    ///
    /// Geometry is computed against a reference grid rather than the real
    /// terminal size: the layout tree is proportional, so relative position —
    /// which is all "is that window to my left" needs — is size-independent.
    pub fn focus_dir(&mut self, dir: Dir) -> bool {
        const REF: usize = 1000;
        let rects = self.layout_rects(REF, REF);
        let Some(&(_, cx, cy, cw, ch)) = rects.iter().find(|(id, ..)| *id == self.current_window)
        else {
            return false;
        };
        let (ccx, ccy) = (cx + cw / 2, cy + ch / 2);

        // Candidates strictly beyond the current window's edge in `dir`, ranked
        // by how close that edge is, then by centre-line offset on the other
        // axis — so `<C-w>j` from a tall left column lands in the window
        // directly below rather than one off to the side.
        let best = rects
            .iter()
            .filter(|(id, ..)| *id != self.current_window)
            .filter_map(|&(id, x, y, w, h)| {
                let (primary, secondary) = match dir {
                    Dir::Left if x + w <= cx => (cx - (x + w), (y + h / 2).abs_diff(ccy)),
                    Dir::Right if x >= cx + cw => (x - (cx + cw), (y + h / 2).abs_diff(ccy)),
                    Dir::Up if y + h <= cy => (cy - (y + h), (x + w / 2).abs_diff(ccx)),
                    Dir::Down if y >= cy + ch => (y - (cy + ch), (x + w / 2).abs_diff(ccx)),
                    _ => return None,
                };
                Some((primary, secondary, id))
            })
            .min();

        match best {
            Some((_, _, id)) => {
                self.current_window = id;
                true
            }
            None => false,
        }
    }

    /// Close every *split* window except the current one (`<C-w>o` /
    /// `:only`). Floats are a separate lifecycle (see [`Editor::floats`]) —
    /// `:only` in real Neovim doesn't touch them either.
    pub fn only_current_window(&mut self) {
        let cur = self.current_window;
        for id in self.layout.windows() {
            if id != cur {
                self.windows[(id.0 - 1) as usize] = None;
            }
        }
        self.layout = Frame::Leaf(cur);
    }

    /// Dump the current buffer as a string (for demos/tests).
    pub fn dump(&self) -> String {
        self.cur_buffer().text.to_string()
    }
}

/// A window-navigation direction (`<C-w>h/j/k/l`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}

impl Default for Editor {
    fn default() -> Self {
        Editor::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_and_read_buffer() {
        let mut ed = Editor::new();
        ed.load_str("hello\nworld", Some("test.txt"));
        assert_eq!(ed.cur_buffer().text.line_count(), 2);
        assert_eq!(ed.cur_buffer().name.as_deref(), Some("test.txt"));
        assert_eq!(ed.cursor(), Position::new(0, 0));
    }

    #[test]
    fn set_cursor_clamps() {
        let mut ed = Editor::new();
        ed.load_str("ab", None);
        ed.set_cursor(Position::new(0, 99));
        assert_eq!(ed.cursor(), Position::new(0, 1)); // last char
    }

    #[test]
    fn split_builds_frame_tree() {
        let mut ed = Editor::new();
        let w2 = ed.split_current(true);
        assert_eq!(ed.layout.windows(), vec![WindowId(1), w2]);
        assert!(matches!(ed.layout, Frame::Row(_)));
    }

    #[test]
    fn options_resolve_through_editor() {
        let mut ed = Editor::new();
        assert_eq!(ed.options().tabstop(), 4);
        ed.cur_buffer_mut().options.tabstop = Some(2);
        assert_eq!(ed.options().tabstop(), 2);
    }

    fn float_config() -> FloatConfig {
        FloatConfig { relative: FloatRelative::Cursor, row: 1, col: 0, width: 40, height: 3, border: true }
    }

    #[test]
    fn a_float_is_not_part_of_the_split_layout() {
        let mut ed = Editor::new();
        let buf = ed.create_buffer(Buffer::new(), None);
        let float_id = ed.open_float(buf, float_config());
        // The float is a real, addressable window...
        assert_eq!(ed.window(float_id).unwrap().buffer, buf);
        // ...but not a leaf in the split tree, so closing "the last window"
        // (the one real split) must still be refused regardless of it.
        assert_eq!(ed.layout.windows(), vec![WindowId(1)]);
        assert!(!ed.close_window(WindowId(1)), "the only split window can't be closed");
    }

    #[test]
    fn window_ids_lists_floats_after_splits() {
        let mut ed = Editor::new();
        let w2 = ed.split_current(true);
        let buf = ed.create_buffer(Buffer::new(), None);
        let float_id = ed.open_float(buf, float_config());
        assert_eq!(ed.window_ids(), vec![WindowId(1), w2, float_id]);
    }

    #[test]
    fn close_float_removes_it_but_not_a_split_window() {
        let mut ed = Editor::new();
        let buf = ed.create_buffer(Buffer::new(), None);
        let float_id = ed.open_float(buf, float_config());
        assert!(ed.close_float(float_id));
        assert!(ed.window(float_id).is_none());
        assert!(ed.float_ids().is_empty());
        // Not a float (it's the real split window) — refuses rather than
        // silently doing nothing useful.
        assert!(!ed.close_float(WindowId(1)));
    }

    #[test]
    fn float_config_is_retrievable_and_absent_for_non_floats() {
        let mut ed = Editor::new();
        let buf = ed.create_buffer(Buffer::new(), None);
        let cfg = float_config();
        let float_id = ed.open_float(buf, cfg);
        assert_eq!(ed.float_config(float_id), Some(&cfg));
        assert_eq!(ed.float_config(WindowId(1)), None);
    }
}
