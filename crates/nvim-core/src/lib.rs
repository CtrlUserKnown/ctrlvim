//! Top-level facade wiring the ctrlvim engine together for an embedding
//! frontend (e.g. a Ratatui UI).
//!
//! The engine is split across crates by concern; this crate re-exports the
//! handful of types a frontend actually drives: [`Session`] for modal editing
//! and key input, and [`Host`] for the Lua-enabled runtime. A real frontend
//! would render the `Session`/`Host` editor state each frame and feed [`Key`]s
//! back in.

pub use nvim_api::ApiContext;
pub use nvim_async::{Event, EventLoop};
pub use nvim_editor::{Editor, Frame, Key, Mode, Session};
pub use nvim_lua::Host;
pub use nvim_types::{BufferId, Object, Position, WindowId};

/// A convenience entry point: an editor session with Lua available.
///
/// For the current milestones the modal editing ([`Session`]) and the Lua host
/// ([`Host`]) own separate `Editor` instances; unifying them (so `:lua` edits
/// and keystrokes share one buffer) is the natural next integration step once
/// the command-line `:lua` bridge lands.
pub struct Nvim {
    pub session: Session,
}

impl Nvim {
    pub fn new() -> Self {
        Nvim {
            session: Session::new(),
        }
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
}

impl Default for Nvim {
    fn default() -> Self {
        Nvim::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn facade_end_to_end_editing() {
        let mut nvim = Nvim::new();
        nvim.open("hello world", Some("scratch"));
        nvim.feed("dw");
        assert_eq!(nvim.lines(), vec!["world"]);
        nvim.feed("ifoo <Esc>");
        assert_eq!(nvim.lines(), vec!["foo world"]);
        assert_eq!(nvim.mode(), "n");
    }
}
