//! Autocommands — the data model of `autocmd.c`.
//!
//! An [`Autocmd`] carries a tagged [`CallbackRef`]: either a Lua callback (held
//! as a [`LuaRef`] — the concrete `mlua::RegistryKey` lives in `nvim-lua`) or an
//! Ex command string. Firing an event collects the matching Lua callbacks and
//! hands them back to the caller; `nvim-lua` resolves each `LuaRef` and invokes
//! it, reusing the one callback mechanism that keymaps/timers also use.

use nvim_types::object::LuaRef;

/// What runs when an autocmd fires.
#[derive(Debug, Clone)]
pub enum CallbackRef {
    /// A Lua function, referenced by registry id.
    Lua(LuaRef),
    /// An Ex command to execute.
    Command(String),
}

/// One registered autocommand.
#[derive(Debug, Clone)]
pub struct Autocmd {
    pub id: u32,
    pub event: String,
    /// File pattern (`*`, `*.rs`, ...). `*` matches everything.
    pub pattern: String,
    pub callback: CallbackRef,
    /// If true, the autocmd deletes itself after firing once.
    pub once: bool,
}

/// The per-editor autocmd registry.
#[derive(Default)]
pub struct AutocmdStore {
    cmds: Vec<Autocmd>,
    next_id: u32,
}

impl AutocmdStore {
    pub fn new() -> Self {
        AutocmdStore {
            cmds: Vec::new(),
            next_id: 1,
        }
    }

    /// Register an autocmd, returning its id (`nvim_create_autocmd`).
    pub fn create(
        &mut self,
        event: impl Into<String>,
        pattern: impl Into<String>,
        callback: CallbackRef,
        once: bool,
    ) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        self.cmds.push(Autocmd {
            id,
            event: event.into(),
            pattern: pattern.into(),
            callback,
            once,
        });
        id
    }

    /// Delete an autocmd by id (`nvim_del_autocmd`).
    pub fn delete(&mut self, id: u32) -> bool {
        let before = self.cmds.len();
        self.cmds.retain(|c| c.id != id);
        self.cmds.len() != before
    }

    /// Collect the callbacks matching `event`/`file`, removing any `once`
    /// autocmds that fire. Returns the callbacks to invoke, in registration
    /// order (Neovim guarantees ordering within a group).
    pub fn fire(&mut self, event: &str, file: &str) -> Vec<CallbackRef> {
        let mut to_run = Vec::new();
        let mut fired_once = Vec::new();
        for cmd in &self.cmds {
            if cmd.event.eq_ignore_ascii_case(event) && pattern_matches(&cmd.pattern, file) {
                to_run.push(cmd.callback.clone());
                if cmd.once {
                    fired_once.push(cmd.id);
                }
            }
        }
        self.cmds.retain(|c| !fired_once.contains(&c.id));
        to_run
    }

    pub fn len(&self) -> usize {
        self.cmds.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cmds.is_empty()
    }
}

/// Very small glob matcher: `*` matches any suffix, a leading `*.ext` matches by
/// extension, otherwise exact match. Full Vim pattern matching is deferred.
fn pattern_matches(pattern: &str, file: &str) -> bool {
    if pattern == "*" || pattern.is_empty() {
        return true;
    }
    if let Some(ext) = pattern.strip_prefix("*.") {
        return file.rsplit('.').next() == Some(ext);
    }
    if let Some(prefix) = pattern.strip_suffix('*') {
        return file.starts_with(prefix);
    }
    pattern == file
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_and_fire() {
        let mut s = AutocmdStore::new();
        let id = s.create("BufWritePre", "*", CallbackRef::Lua(LuaRef(7)), false);
        assert_eq!(id, 1);
        let fired = s.fire("BufWritePre", "foo.txt");
        assert_eq!(fired.len(), 1);
        assert!(matches!(fired[0], CallbackRef::Lua(LuaRef(7))));
    }

    #[test]
    fn pattern_filtering() {
        let mut s = AutocmdStore::new();
        s.create("BufEnter", "*.rs", CallbackRef::Command("echo".into()), false);
        assert_eq!(s.fire("BufEnter", "main.rs").len(), 1);
        assert_eq!(s.fire("BufEnter", "main.py").len(), 0);
    }

    #[test]
    fn once_autocmd_removes_itself() {
        let mut s = AutocmdStore::new();
        s.create("VimEnter", "*", CallbackRef::Lua(LuaRef(1)), true);
        assert_eq!(s.fire("VimEnter", "").len(), 1);
        assert_eq!(s.fire("VimEnter", "").len(), 0);
        assert!(s.is_empty());
    }

    #[test]
    fn event_name_is_case_insensitive() {
        let mut s = AutocmdStore::new();
        s.create("bufwritepre", "*", CallbackRef::Lua(LuaRef(1)), false);
        assert_eq!(s.fire("BufWritePre", "x").len(), 1);
    }
}
