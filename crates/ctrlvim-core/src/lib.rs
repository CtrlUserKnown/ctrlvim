//! Top-level facade wiring the ctrlvim engine together for an embedding
//! frontend (e.g. a Ratatui UI).
//!
//! The engine is split across crates by concern; this crate re-exports the
//! handful of types a frontend actually drives: [`Session`] for modal editing
//! and key input, and [`Host`] for the Lua-enabled runtime. A real frontend
//! would render the `Session`/`Host` editor state each frame and feed [`Key`]s
//! back in.

pub mod syntax;

pub use ctrlvim_api::ApiContext;
pub use ctrlvim_treesitter::{HlKind, HlSpan};
pub use ctrlvim_async::{Event, EventLoop, Jobs, LineBuffer, TimerService};
pub use ctrlvim_editor::{
    ex_commands, is_ex_command, BufferCmd, Editor, ExCommand, ExEffect, Fold, Folds, Frame, Key,
    Mode, QfItem, QfKind, QuickfixCmd, QuickfixList, Selection, Session, TagCmd, VisualKind,
};
pub use ctrlvim_editor::fold::fold_text;
pub use ctrlvim_editor::tags::{resolve_address as resolve_tag_address, TagAddress, TagTable};
pub use ctrlvim_editor::quickfix::{grep_text, Matcher, OutputParser};
pub use ctrlvim_lua::Host;
pub use ctrlvim_types::{BufferId, Object, Position, WindowId};

/// A convenience entry point: an editor session with Lua available.
///
/// For the current milestones the modal editing ([`Session`]) and the Lua host
/// ([`Host`]) own separate `Editor` instances; unifying them (so `:lua` edits
/// and keystrokes share one buffer) is the natural next integration step once
/// the command-line `:lua` bridge lands.
pub struct Ctrlvim {
    pub session: Session,
    /// Persistent Vimscript state (`g:` variables, `:function` defs).
    script: ctrlvim_vimscript::ScriptState,
    /// The Lua host, created on first `:lua`. It owns its own `Editor`; we sync
    /// the session's buffer text in and out around each run.
    host: Option<Host>,
}

impl Ctrlvim {
    pub fn new() -> Self {
        Ctrlvim {
            session: Session::new(),
            script: ctrlvim_vimscript::ScriptState::default(),
            host: None,
        }
    }

    /// Run a line (or file) of Vimscript against the live buffer. Returns any
    /// `:echo` output. Commits an undo checkpoint if the buffer changed.
    pub fn run_vimscript(&mut self, src: &str) -> Result<Vec<String>, String> {
        self.script.output.clear();
        let before = self.session.editor.cur_buffer().text.lines();
        let result = {
            let mut interp = ctrlvim_vimscript::Interp::new(&mut self.script, &mut self.session.editor);
            interp.run(src).map_err(|e| e.to_string())
        };
        if self.session.editor.cur_buffer().text.lines() != before {
            self.session.checkpoint_undo();
        }
        result.map(|()| std::mem::take(&mut self.script.output))
    }

    /// Run a Lua chunk against the live buffer (syncing text in and out).
    pub fn run_lua(&mut self, code: &str) -> Result<(), String> {
        if self.host.is_none() {
            self.host = Some(Host::new(Editor::new()).map_err(|e| e.to_string())?);
        }
        let host = self.host.as_ref().unwrap();
        let text = self.lines().join("\n");
        host.with_editor_mut(|ed| ed.load_str(&text, None));
        let result = host.exec(code).map_err(|e| e.to_string());
        let new_lines = host.with_editor(|ed| ed.cur_buffer().text.lines());
        if new_lines != self.session.editor.cur_buffer().text.lines() {
            let count = self.session.editor.cur_buffer().text.line_count();
            self.session
                .editor
                .cur_buffer_mut()
                .text
                .set_lines(0, count, &new_lines);
            self.session.checkpoint_undo();
        }
        result
    }

    /// Open a file's contents into the current buffer.
    pub fn open(&mut self, contents: &str, name: Option<&str>) {
        self.session.editor.load_str(contents, name);
    }

    /// Feed a `<...>`-encoded key sequence (e.g. `"dw"`, `"ifoo<Esc>"`).
    pub fn feed(&mut self, keys: &str) {
        self.session.feed_str(keys);
    }

    /// Current buffer lines.
    pub fn lines(&self) -> Vec<String> {
        self.session.lines()
    }

    /// Cursor as `(1-based row, 0-based col)`.
    pub fn cursor(&self) -> (i64, i64) {
        self.session.cursor().to_cursor_api()
    }

    /// Current mode short name.
    pub fn mode(&self) -> &'static str {
        self.session.mode_name()
    }

    /// The active visual selection (normalized), or `None` outside Visual mode.
    pub fn selection(&self) -> Option<Selection> {
        self.session.selection()
    }

    /// Char-column `(start, end)` ranges of `hlsearch` matches on `line`.
    pub fn search_line_matches(&self, line: usize) -> Vec<(usize, usize)> {
        self.session.search_line_matches(line)
    }

    /// The in-progress `:` command line to render, or `None` outside Cmdline mode.
    pub fn cmdline(&self) -> Option<String> {
        self.session.cmdline()
    }

    /// A short display of any partially-typed `<leader>` chord (for the status
    /// line); empty when nothing is pending.
    pub fn pending_display(&self) -> String {
        self.session.pending_display()
    }

    /// Drain the host effects requested by Ex commands (`:w`/`:q`/…) since the
    /// last call, for the frontend to perform.
    pub fn take_effects(&mut self) -> Vec<ExEffect> {
        self.session.take_effects()
    }

    /// Whether the current buffer has unsaved changes (`'modified'`).
    pub fn is_modified(&self) -> bool {
        self.session.is_modified()
    }

    /// Carry per-buffer dirty state across the single-buffer facade.
    pub fn set_modified(&mut self, modified: bool) {
        self.session.set_modified(modified);
    }
}

impl Default for Ctrlvim {
    fn default() -> Self {
        Ctrlvim::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn facade_end_to_end_editing() {
        let mut ctrlvim = Ctrlvim::new();
        ctrlvim.open("hello world", Some("scratch"));
        ctrlvim.feed("dw");
        assert_eq!(ctrlvim.lines(), vec!["world"]);
        ctrlvim.feed("ifoo <Esc>");
        assert_eq!(ctrlvim.lines(), vec!["foo world"]);
        assert_eq!(ctrlvim.mode(), "n");
    }
}

