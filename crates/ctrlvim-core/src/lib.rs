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
    ex_commands, is_ex_command, AiCmd, BufferCmd, Editor, ExCommand, ExEffect, Fold, Folds, Frame,
    Key, Mode, Mods, PinCmd, QfItem, QfKind, QuickfixCmd, QuickfixList, Selection, Session, SpecialKey,
    TagCmd, VisualKind,
};
pub use ctrlvim_editor::suggest::{
    Accept as SuggestAccept, ContextWindow, InlineSuggest, SuggestRequest, Suggestion,
};
pub use ctrlvim_editor::keymap::{
    notation as keys_notation, Continuation, Keymap, KeymapMatch, MapMode, Mapping,
};
pub use ctrlvim_editor::fold::fold_text;
pub use ctrlvim_editor::tags::{resolve_address as resolve_tag_address, TagAddress, TagTable};
pub use ctrlvim_editor::quickfix::{grep_text, Matcher, OutputParser};
pub use ctrlvim_editor::pinned::PinList;
pub use ctrlvim_editor::replace::{ReplaceHit, ReplacePlan};
pub use ctrlvim_lua::Host;
pub use ctrlvim_types::{BufferId, Object, Position, WindowId};
pub use ctrlvim_text::{char_index_at, char_width, display_width, width_upto};

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
        self.run_lua_as(None, code)
    }

    /// Run a Lua chunk as a named plugin script: `source` (e.g. a plugin's
    /// file stem) tags any `ctrlvim_create_user_command` calls it makes, so
    /// the frontend can attribute the resulting palette entries to it. Used
    /// by startup plugin loading; `run_lua` is the `source: None` case for
    /// ad hoc `:lua` chunks.
    pub fn run_lua_as(&mut self, source: Option<&str>, code: &str) -> Result<(), String> {
        if self.host.is_none() {
            self.host = Some(Host::new(Editor::new()).map_err(|e| e.to_string())?);
        }
        let host = self.host.as_ref().unwrap();
        let text = self.lines().join("\n");
        host.with_editor_mut(|ed| ed.load_str(&text, None));
        host.set_current_source(source.map(str::to_string));
        let result = host.exec(code).map_err(|e| e.to_string());
        host.set_current_source(None);
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

    /// Plugin-registered commands (`vim.api.ctrlvim_create_user_command`),
    /// name, description, and source script (if any), for the frontend's
    /// command palette. Empty until a plugin has actually run (the Lua host
    /// is created lazily on first `:lua`/plugin load).
    pub fn plugin_commands(&self) -> Vec<(String, String, Option<String>)> {
        match &self.host {
            Some(host) => host.user_commands(),
            None => Vec::new(),
        }
    }

    /// Run a plugin-registered command by name (selecting it from the
    /// palette), syncing buffer text in/out the same way [`run_lua`] does.
    /// Returns whether a command with that name was found.
    ///
    /// [`run_lua`]: Self::run_lua
    pub fn run_plugin_command(&mut self, name: &str) -> Result<bool, String> {
        let Some(host) = self.host.as_ref() else { return Ok(false) };
        let text = self.lines().join("\n");
        host.with_editor_mut(|ed| ed.load_str(&text, None));
        let ran = host.run_user_command(name).map_err(|e| e.to_string())?;
        let new_lines = host.with_editor(|ed| ed.cur_buffer().text.lines());
        if new_lines != self.session.editor.cur_buffer().text.lines() {
            let count = self.session.editor.cur_buffer().text.line_count();
            self.session.editor.cur_buffer_mut().text.set_lines(0, count, &new_lines);
            self.session.checkpoint_undo();
        }
        Ok(ran)
    }

    /// Fire an autocommand event at the Lua host, so callbacks registered with
    /// `ctrlvim_create_autocmd` actually run.
    ///
    /// This is a no-op when no Lua has been executed yet: the host is created
    /// lazily, and with no host there are no callbacks to notify. It must be
    /// called from the *real* editing paths (write, buffer switch, startup) —
    /// autocmds that only fire from a demo binary are not a feature.
    pub fn fire_autocmd(&mut self, event: &str, file: &str) {
        if let Some(host) = self.host.as_ref() {
            let _ = host.fire_autocmd(event, file);
        }
    }

    /// Whether a Lua host exists yet (i.e. any Lua has run).
    pub fn has_host(&self) -> bool {
        self.host.is_some()
    }

    /// Create the Lua host if it doesn't exist, so plugins can be loaded and
    /// autocmds registered before any `:lua` is typed.
    pub fn ensure_host(&mut self) -> Result<(), String> {
        if self.host.is_none() {
            self.host = Some(Host::new(Editor::new()).map_err(|e| e.to_string())?);
        }
        Ok(())
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

    #[test]
    fn plugin_commands_are_empty_before_any_lua_runs() {
        let ctrlvim = Ctrlvim::new();
        assert_eq!(ctrlvim.plugin_commands(), Vec::<(String, String, Option<String>)>::new());
    }

    #[test]
    fn plugin_command_registers_lists_and_edits_the_shared_buffer() {
        let mut ctrlvim = Ctrlvim::new();
        ctrlvim.open("hello world", Some("scratch"));
        ctrlvim
            .run_lua(
                r#"
                vim.api.ctrlvim_create_user_command('Shout', function()
                    vim.api.ctrlvim_set_current_line(vim.api.ctrlvim_get_current_line():upper())
                end, { desc = 'uppercase the line' })
                "#,
            )
            .unwrap();
        assert_eq!(
            ctrlvim.plugin_commands(),
            vec![("Shout".to_string(), "uppercase the line".to_string(), None)]
        );
        assert!(ctrlvim.run_plugin_command("Shout").unwrap());
        assert_eq!(ctrlvim.lines(), vec!["HELLO WORLD"]);
        assert!(!ctrlvim.run_plugin_command("NoSuchCommand").unwrap());
    }

    #[test]
    fn run_lua_as_tags_registered_commands_with_their_source() {
        let mut ctrlvim = Ctrlvim::new();
        ctrlvim
            .run_lua_as(
                Some("my-plugin"),
                "vim.api.ctrlvim_create_user_command('Greet', function() end, {})",
            )
            .unwrap();
        assert_eq!(
            ctrlvim.plugin_commands(),
            vec![("Greet".to_string(), String::new(), Some("my-plugin".to_string()))]
        );
    }
}

