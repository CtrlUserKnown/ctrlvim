//! The central [`Editor`] context — the replacement for Neovim's global
//! `curbuf`/`curwin`/`curtab` triad and the ~590 globals around them.
//!
//! Every operation that C implicitly performs on "the current buffer" becomes a
//! method on `Editor` taking explicit handles. Buffers and windows live in
//! arenas indexed by `Copy` handles, so a stale handle is a clean `None` rather
//! than a dangling pointer.

use crate::window::{Frame, Window};
use ctrlvim_options::{BufferOptions, GlobalOptions, OptionContext};
use ctrlvim_text::{Buffer, MarkStore, Registers, UndoTree};
use ctrlvim_types::{BufferId, Position, WindowId};

/// Everything the editor knows about one buffer. Combines the text engine
/// pieces (`ctrlvim-text`) with editor-level metadata.
pub struct BufferState {
    pub text: Buffer,
    pub undo: UndoTree,
    pub marks: MarkStore,
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

/// The editor context.
pub struct Editor {
    buffers: Vec<Option<BufferState>>,
    windows: Vec<Option<Window>>,
    current_window: WindowId,
    /// Split layout for the (single, for now) tabpage.
    pub layout: Frame,
    pub registers: Registers,
    pub global_options: GlobalOptions,
}

impl Editor {
    /// Create an editor with a single empty buffer shown in a single window.
    pub fn new() -> Self {
        let buf = BufferState::new(Buffer::new(), None);
        let win = Window::new(BufferId(0));
        Editor {
            buffers: vec![Some(buf)],
            windows: vec![Some(win)],
            current_window: WindowId(0),
            layout: Frame::Leaf(WindowId(0)),
            registers: Registers::new(),
            global_options: GlobalOptions::default(),
        }
    }

    /// Load `text` into the current buffer (as if opening a file).
    pub fn load_str(&mut self, text: &str, name: Option<&str>) {
        let bid = self.current_buffer_id();
        let buf = Buffer::from_str(text);
        let state = BufferState::new(buf, name.map(str::to_string));
        self.buffers[bid.0 as usize] = Some(state);
        self.window_mut(self.current_window).unwrap().cursor = Position::default();
    }

    /// Create a new buffer, returning its id.
    pub fn create_buffer(&mut self, text: Buffer, name: Option<String>) -> BufferId {
        let id = BufferId(self.buffers.len() as u32);
        self.buffers.push(Some(BufferState::new(text, name)));
        id
    }

    // --- handle resolution ---

    pub fn current_window_id(&self) -> WindowId {
        self.current_window
    }

    pub fn current_buffer_id(&self) -> BufferId {
        self.window(self.current_window).unwrap().buffer
    }

    pub fn window(&self, id: WindowId) -> Option<&Window> {
        self.windows.get(id.0 as usize).and_then(|w| w.as_ref())
    }

    pub fn window_mut(&mut self, id: WindowId) -> Option<&mut Window> {
        self.windows.get_mut(id.0 as usize).and_then(|w| w.as_mut())
    }

    pub fn buffer(&self, id: BufferId) -> Option<&BufferState> {
        self.buffers.get(id.0 as usize).and_then(|b| b.as_ref())
    }

    pub fn buffer_mut(&mut self, id: BufferId) -> Option<&mut BufferState> {
        self.buffers.get_mut(id.0 as usize).and_then(|b| b.as_mut())
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
        let cur = self.current_window;
        let bufid = self.window(cur).unwrap().buffer;
        let new_id = WindowId(self.windows.len() as u32);
        self.windows.push(Some(Window::new(bufid)));
        self.layout = Self::split_in_frame(std::mem::replace(&mut self.layout, Frame::Leaf(cur)), cur, new_id, vertical);
        new_id
    }

    fn split_in_frame(frame: Frame, target: WindowId, new_id: WindowId, vertical: bool) -> Frame {
        match frame {
            Frame::Leaf(id) if id == target => {
                let pair = vec![Frame::Leaf(id), Frame::Leaf(new_id)];
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
                    .map(|c| Self::split_in_frame(c, target, new_id, vertical))
                    .collect(),
            ),
            Frame::Col(children) => Frame::Col(
                children
                    .into_iter()
                    .map(|c| Self::split_in_frame(c, target, new_id, vertical))
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

    /// All window ids in layout order (`ctrlvim_list_wins`).
    pub fn window_ids(&self) -> Vec<WindowId> {
        self.layout.windows()
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
                if (id.0 as usize) < self.windows.len() {
                    self.windows[id.0 as usize] = None;
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

    /// Dump the current buffer as a string (for demos/tests).
    pub fn dump(&self) -> String {
        self.cur_buffer().text.to_string()
    }
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
        assert_eq!(ed.layout.windows(), vec![WindowId(0), w2]);
        assert!(matches!(ed.layout, Frame::Row(_)));
    }

    #[test]
    fn options_resolve_through_editor() {
        let mut ed = Editor::new();
        assert_eq!(ed.options().tabstop(), 8);
        ed.cur_buffer_mut().options.tabstop = Some(2);
        assert_eq!(ed.options().tabstop(), 2);
    }
}
