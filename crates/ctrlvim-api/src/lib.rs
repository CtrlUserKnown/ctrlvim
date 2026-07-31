//! The Neovim API surface, reimplemented in Rust.
//!
//! [`ApiContext`] is the state every API function operates on (the editor plus
//! API-level registries like autocmds and namespaces). Functions are defined in
//! [`functions`] with `#[ctrlvim_api]`; [`registry`] collects them and [`call`]
//! dispatches by name — the single path shared by the Lua binding (`ctrlvim-lua`)
//! and the msgpack-RPC handler (`ctrlvim-async`).

pub mod autocmd;
pub mod convert;
pub mod functions;
pub mod registry;

use autocmd::AutocmdStore;
use ctrlvim_editor::{Editor, Session};
use ctrlvim_types::object::LuaRef;
use ctrlvim_types::BufferId;
use std::collections::HashMap;

pub use registry::{call, ApiFunction};

/// The context threaded through every API call. Replaces the implicit global
/// state (`curbuf`/`curwin`) plus API-level bookkeeping that Neovim keeps in
/// scattered globals.
///
/// It owns the [`Session`] (editor + modal state) so that Lua/RPC API calls and
/// interactive key input operate on the *same* editor — the unification the
/// plugin-integration milestone needs.
pub struct ApiContext {
    pub session: Session,
    pub autocmds: AutocmdStore,
    /// Callback keymaps registered from Lua (`vim.keymap.set` with a function
    /// right-hand side): (mode, lhs) -> (LuaRef id, `desc`).
    ///
    /// A *string* right-hand side doesn't land here — it goes straight into
    /// [`session`](Self::session)'s mapping table, where it works like any
    /// other mapping. Callbacks still need the typeahead layer to learn how to
    /// invoke a `LuaRef`, so for now they are trigger-only; keeping the `desc`
    /// means the description survives until it can be listed.
    pub keymaps: HashMap<(String, String), (i64, Option<String>)>,
    /// Commands registered from Lua (`vim.api.ctrlvim_create_user_command`):
    /// name -> (callback, description, source). `source` is whatever script
    /// was executing at registration time (see [`current_source`]), so the
    /// frontend can attribute a command to the plugin that contributed it.
    /// This is what makes a plugin's commands show up in the frontend's
    /// command palette alongside the engine's own — the same unification
    /// `:command` gets for Vimscript.
    ///
    /// [`current_source`]: Self::current_source
    pub user_commands: HashMap<String, (LuaRef, String, Option<String>)>,
    /// The script currently being sourced (a plugin's file stem, e.g.
    /// `"my-plugin"`), if any — set by the host around a plugin load so
    /// `ctrlvim_create_user_command` can tag its registrations. `None` for
    /// ad hoc `:lua` chunks with no associated file.
    pub current_source: Option<String>,
    /// Persistent Vimscript state (globals + user functions) backing `vim.fn`.
    pub script: ctrlvim_vimscript::ScriptState,
    /// `nvim_buf_attach` registrations: buffer -> (`on_lines` callback, its
    /// last-seen line snapshot, used to compute the changed range on the
    /// next [`Self::check_buf_watcher`] call). One watcher per buffer, since
    /// that's all `vim.lsp`'s own attach path needs.
    pub buf_watchers: HashMap<BufferId, (LuaRef, Vec<String>)>,
    namespaces: HashMap<String, u32>,
    next_namespace: u32,
    augroups: HashMap<String, u32>,
    next_augroup: u32,
}

impl ApiContext {
    pub fn new(editor: Editor) -> Self {
        ApiContext {
            session: Session::from_editor(editor),
            autocmds: AutocmdStore::new(),
            keymaps: HashMap::new(),
            user_commands: HashMap::new(),
            current_source: None,
            script: ctrlvim_vimscript::ScriptState::default(),
            buf_watchers: HashMap::new(),
            namespaces: HashMap::new(),
            next_namespace: 1,
            augroups: HashMap::new(),
            next_augroup: 1,
        }
    }

    /// Call a Vimscript builtin or user function through the persistent
    /// interpreter, against the shared editor. Backs `vim.fn.*` / `vim.call`.
    pub fn call_vimfn(&mut self, name: &str, args: Vec<ctrlvim_types::Object>) -> ctrlvim_types::Result<ctrlvim_types::Object> {
        // Disjoint field borrows: the interpreter needs both the script state
        // and the editor, which are separate fields of `self`.
        let editor = &mut self.session.editor;
        let mut interp = ctrlvim_vimscript::Interp::new(&mut self.script, editor);
        interp.call_function(name, args)
    }

    /// Run a Vimscript source chunk (`:source` / `ctrlvim_exec`-style).
    pub fn exec_vimscript(&mut self, src: &str) -> ctrlvim_types::Result<()> {
        let editor = &mut self.session.editor;
        let mut interp = ctrlvim_vimscript::Interp::new(&mut self.script, editor);
        interp.run(src)
    }

    /// Run an Ex command through the session, exactly as typing it would.
    ///
    /// `:set` and `:map` are Ex commands, not Vimscript, so they must go here
    /// rather than through [`Self::exec_vimscript`] — the interpreter would
    /// treat `set number` as an expression and fail.
    pub fn exec_ex(&mut self, cmd: &str) {
        self.session.feed_str(":");
        for c in cmd.chars() {
            self.session.feed(ctrlvim_editor::Key::Char(c));
        }
        self.session.feed(ctrlvim_editor::Key::Enter);
    }

    /// Read a Vimscript global (`g:name`), backing `vim.g.name`.
    pub fn get_global(&self, name: &str) -> Option<ctrlvim_types::Object> {
        self.script.globals.get(name).cloned()
    }

    /// Set a Vimscript global, backing `vim.g.name = value`.
    pub fn set_global(&mut self, name: &str, value: ctrlvim_types::Object) {
        self.script.globals.insert(name.to_string(), value);
    }

    /// Shared read access to the editor.
    pub fn editor(&self) -> &Editor {
        &self.session.editor
    }

    /// Shared mutable access to the editor.
    pub fn editor_mut(&mut self) -> &mut Editor {
        &mut self.session.editor
    }

    /// Allocate or reuse a namespace id (`ctrlvim_create_namespace`).
    pub fn create_namespace(&mut self, name: &str) -> u32 {
        if name.is_empty() {
            // Anonymous namespace: always a fresh id.
            let id = self.next_namespace;
            self.next_namespace += 1;
            return id;
        }
        if let Some(id) = self.namespaces.get(name) {
            return *id;
        }
        let id = self.next_namespace;
        self.next_namespace += 1;
        self.namespaces.insert(name.to_string(), id);
        id
    }

    /// Every *named* namespace created so far (`nvim_get_namespaces`).
    /// Anonymous namespaces (created with an empty name) are deliberately
    /// excluded, same as real Neovim — they're never in this map by name.
    pub fn namespaces(&self) -> &HashMap<String, u32> {
        &self.namespaces
    }

    /// Allocate or reuse an autocmd-group id (`nvim_create_augroup`). Unlike
    /// real Neovim, autocmds aren't actually tagged by group yet — a group
    /// id is real and stable per name, but `opts.clear` (real Neovim: delete
    /// the group's existing autocmds first) is a no-op, since there's
    /// nothing to attribute to a group to clear. Harmless for the common
    /// case (a plugin defining its augroup once, at load time, before it has
    /// registered anything into it) — a plugin that *redefines* the same
    /// augroup expecting old autocmds to disappear would see them pile up
    /// instead, a real but narrow gap.
    pub fn create_augroup(&mut self, name: &str) -> u32 {
        if name.is_empty() {
            let id = self.next_augroup;
            self.next_augroup += 1;
            return id;
        }
        if let Some(id) = self.augroups.get(name) {
            return *id;
        }
        let id = self.next_augroup;
        self.next_augroup += 1;
        self.augroups.insert(name.to_string(), id);
        id
    }

    /// If `buf` has an `nvim_buf_attach` watcher and its content has changed
    /// since the last check, return `(callback, changedtick, firstline,
    /// lastline, new_lastline)` and update the stored snapshot — the
    /// `ctrlvim-lua` host resolves the callback and invokes it. `None` if
    /// there's no watcher, or nothing changed.
    ///
    /// The range is a full-buffer diff (first and last differing lines),
    /// not true incremental tracking — real Neovim also computes this from
    /// the (single) buffer state, so callers reacting to the range
    /// (`vim.lsp`'s change tracking) work the same either way, just doing
    /// slightly more work when only a small edit happened.
    pub fn check_buf_watcher(&mut self, buf: BufferId) -> Option<(LuaRef, u64, usize, usize, usize)> {
        let current = self.editor().buffer(buf)?.text.lines();
        let tick = self.editor().buffer(buf)?.changedtick;
        let (luaref, last) = self.buf_watchers.get_mut(&buf)?;
        if *last == current {
            return None;
        }
        let first = last.iter().zip(current.iter()).take_while(|(a, b)| a == b).count();
        let old_tail = last.len() - first;
        let new_tail = current.len() - first;
        let common_tail = last[first..]
            .iter()
            .rev()
            .zip(current[first..].iter().rev())
            .take_while(|(a, b)| a == b)
            .count()
            .min(old_tail.min(new_tail));
        let lastline = last.len() - common_tail;
        let new_lastline = current.len() - common_tail;
        let luaref = *luaref;
        *last = current;
        Some((luaref, tick, first, lastline, new_lastline))
    }
}

impl Default for ApiContext {
    fn default() -> Self {
        ApiContext::new(Editor::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ctrlvim_types::Object;
    use std::collections::BTreeMap;

    fn ctx_with(text: &str) -> ApiContext {
        let mut ed = Editor::new();
        ed.load_str(text, None);
        ApiContext::new(ed)
    }

    #[test]
    fn macro_registered_functions_are_discoverable() {
        // The proc-macro's inventory submissions must be visible at runtime.
        assert!(registry::count() >= 8);
        assert!(registry::lookup("ctrlvim_get_current_line").is_some());
    }

    #[test]
    fn dispatch_get_current_line() {
        let mut cx = ctx_with("hello world");
        let out = call(&mut cx, "ctrlvim_get_current_line", &[]).unwrap();
        assert_eq!(out.as_str(), Some("hello world"));
    }

    #[test]
    fn dispatch_set_current_line_with_arg_conversion() {
        let mut cx = ctx_with("old");
        call(&mut cx, "ctrlvim_set_current_line", &[Object::str("new text")]).unwrap();
        assert_eq!(cx.editor().cur_buffer().text.line(0).as_deref(), Some("new text"));
    }

    #[test]
    fn dispatch_type_error_surfaces() {
        let mut cx = ctx_with("x");
        // set_current_line expects a string; passing an integer must error.
        let err = call(&mut cx, "ctrlvim_set_current_line", &[Object::Integer(3)]).unwrap_err();
        assert!(format!("{err}").contains("expected string"));
    }

    #[test]
    fn win_cursor_roundtrip() {
        let mut cx = ctx_with("line one\nline two");
        let pos = Object::Array(vec![Object::Integer(2), Object::Integer(3)]);
        call(&mut cx, "ctrlvim_win_set_cursor", &[pos]).unwrap();
        let got = call(&mut cx, "ctrlvim_win_get_cursor", &[]).unwrap();
        assert_eq!(got, Object::Array(vec![Object::Integer(2), Object::Integer(3)]));
    }

    #[test]
    fn create_autocmd_stores_lua_callback() {
        let mut cx = ctx_with("x");
        let mut opts = BTreeMap::new();
        opts.insert("callback".to_string(), Object::LuaRef(ctrlvim_types::object::LuaRef(42)));
        opts.insert("pattern".to_string(), Object::str("*.rs"));
        let id = call(&mut cx, "ctrlvim_create_autocmd", &[Object::str("BufWritePre"), Object::Dict(opts)]).unwrap();
        assert_eq!(id, Object::Integer(1));
        let fired = cx.autocmds.fire("BufWritePre", "main.rs");
        assert_eq!(fired.len(), 1);
    }

    #[test]
    fn namespace_reuse() {
        let mut cx = ctx_with("x");
        let a = call(&mut cx, "ctrlvim_create_namespace", &[Object::str("mine")]).unwrap();
        let b = call(&mut cx, "ctrlvim_create_namespace", &[Object::str("mine")]).unwrap();
        assert_eq!(a, b);
    }

    fn buf_id(o: &Object) -> ctrlvim_types::BufferId {
        match o {
            Object::Buffer(b) => *b,
            _ => panic!("expected a buffer handle, got {o:?}"),
        }
    }

    fn win_id(o: &Object) -> ctrlvim_types::WindowId {
        match o {
            Object::Window(w) => *w,
            _ => panic!("expected a window handle, got {o:?}"),
        }
    }

    #[test]
    fn nvim_buf_functions_take_an_explicit_handle() {
        let mut cx = ctx_with("first\nsecond");
        let buf = call(&mut cx, "nvim_get_current_buf", &[]).unwrap();

        let lines = call(
            &mut cx,
            "nvim_buf_get_lines",
            &[buf.clone(), Object::Integer(0), Object::Integer(-1), Object::Boolean(false)],
        )
        .unwrap();
        assert_eq!(lines, Object::Array(vec![Object::str("first"), Object::str("second")]));

        call(
            &mut cx,
            "nvim_buf_set_lines",
            &[
                buf.clone(),
                Object::Integer(0),
                Object::Integer(1),
                Object::Boolean(false),
                Object::Array(vec![Object::str("changed")]),
            ],
        )
        .unwrap();
        assert_eq!(cx.editor().buffer(buf_id(&buf)).unwrap().text.line(0).as_deref(), Some("changed"));

        let count = call(&mut cx, "nvim_buf_line_count", &[buf.clone()]).unwrap();
        assert_eq!(count, Object::Integer(2));

        assert_eq!(call(&mut cx, "nvim_buf_is_valid", &[buf]).unwrap(), Object::Boolean(true));
        assert_eq!(
            call(&mut cx, "nvim_buf_is_valid", &[Object::Integer(999)]).unwrap(),
            Object::Boolean(false)
        );
    }

    #[test]
    fn nvim_win_functions_take_an_explicit_handle() {
        let mut cx = ctx_with("one\ntwo\nthree");
        let win = call(&mut cx, "nvim_get_current_win", &[]).unwrap();

        call(&mut cx, "nvim_win_set_cursor", &[win.clone(), Object::Array(vec![Object::Integer(2), Object::Integer(1)])])
            .unwrap();
        let cursor = call(&mut cx, "nvim_win_get_cursor", &[win.clone()]).unwrap();
        assert_eq!(cursor, Object::Array(vec![Object::Integer(2), Object::Integer(1)]));

        let buf = call(&mut cx, "nvim_win_get_buf", &[win.clone()]).unwrap();
        assert_eq!(buf, Object::Buffer(cx.editor().current_buffer_id()));

        assert_eq!(call(&mut cx, "nvim_win_is_valid", &[win]).unwrap(), Object::Boolean(true));
        assert_eq!(
            call(&mut cx, "nvim_win_is_valid", &[Object::Integer(999)]).unwrap(),
            Object::Boolean(false)
        );
    }

    #[test]
    fn nvim_open_win_creates_a_float_not_in_the_split_layout() {
        let mut cx = ctx_with("x");
        let buf = call(&mut cx, "nvim_get_current_buf", &[]).unwrap();
        let mut config = std::collections::BTreeMap::new();
        config.insert("relative".to_string(), Object::str("cursor"));
        config.insert("width".to_string(), Object::Integer(30));
        config.insert("height".to_string(), Object::Integer(2));
        config.insert("row".to_string(), Object::Integer(1));

        let win = call(&mut cx, "nvim_open_win", &[buf, Object::Boolean(false), Object::Dict(config)]).unwrap();
        let win_id_val = win_id(&win);

        // Real window, addressable like any other...
        assert_eq!(call(&mut cx, "nvim_win_is_valid", &[win.clone()]).unwrap(), Object::Boolean(true));
        // ...but not part of the split layout.
        assert!(!cx.editor().window_ids()[..cx.editor().layout.windows().len()].contains(&win_id_val));
        assert!(cx.editor().float_ids().contains(&win_id_val));

        // enter=false: focus didn't move to the float.
        assert_ne!(cx.editor().current_window_id(), win_id_val);

        call(&mut cx, "nvim_win_close", &[win, Object::Boolean(false)]).unwrap();
        assert!(cx.editor().float_ids().is_empty());
    }

    #[test]
    fn nvim_open_win_with_enter_focuses_the_float() {
        let mut cx = ctx_with("x");
        let buf = call(&mut cx, "nvim_get_current_buf", &[]).unwrap();
        let mut config = std::collections::BTreeMap::new();
        config.insert("width".to_string(), Object::Integer(10));
        config.insert("height".to_string(), Object::Integer(1));

        let win = call(&mut cx, "nvim_open_win", &[buf, Object::Boolean(true), Object::Dict(config)]).unwrap();
        assert_eq!(cx.editor().current_window_id(), win_id(&win));
    }

    #[test]
    fn nvim_open_win_rejects_relative_to_another_window() {
        let mut cx = ctx_with("x");
        let buf = call(&mut cx, "nvim_get_current_buf", &[]).unwrap();
        let mut config = std::collections::BTreeMap::new();
        config.insert("relative".to_string(), Object::str("win"));
        config.insert("width".to_string(), Object::Integer(10));
        config.insert("height".to_string(), Object::Integer(1));
        let err = call(&mut cx, "nvim_open_win", &[buf, Object::Boolean(false), Object::Dict(config)]).unwrap_err();
        assert!(format!("{err}").contains("relative=win"));
    }

    #[test]
    fn nvim_win_close_refuses_the_last_split_window() {
        let mut cx = ctx_with("x");
        let win = call(&mut cx, "nvim_get_current_win", &[]).unwrap();
        assert!(call(&mut cx, "nvim_win_close", &[win, Object::Boolean(false)]).is_err());
    }

    #[test]
    fn nvim_buf_set_extmark_stores_decoration_opts_and_details_round_trip() {
        let mut cx = ctx_with("line one\nline two");
        let buf = call(&mut cx, "nvim_get_current_buf", &[]).unwrap();
        let ns = call(&mut cx, "nvim_create_namespace", &[Object::str("diag")]).unwrap();

        let mut opts = std::collections::BTreeMap::new();
        opts.insert("end_row".to_string(), Object::Integer(0));
        opts.insert("end_col".to_string(), Object::Integer(4));
        opts.insert("hl_group".to_string(), Object::str("DiagnosticError"));
        opts.insert(
            "virt_text".to_string(),
            Object::Array(vec![Object::Array(vec![Object::str("bad token"), Object::str("Comment")])]),
        );
        let id = call(
            &mut cx,
            "nvim_buf_set_extmark",
            &[buf.clone(), ns.clone(), Object::Integer(0), Object::Integer(0), Object::Dict(opts)],
        )
        .unwrap();

        // Bare position lookup, no details.
        let pos = call(&mut cx, "nvim_buf_get_extmark_by_id", &[buf.clone(), ns.clone(), id.clone(), Object::Nil]).unwrap();
        assert_eq!(pos, Object::Array(vec![Object::Integer(0), Object::Integer(0)]));

        // With details.
        let mut details_opts = std::collections::BTreeMap::new();
        details_opts.insert("details".to_string(), Object::Boolean(true));
        let with_details = call(
            &mut cx,
            "nvim_buf_get_extmark_by_id",
            &[buf.clone(), ns.clone(), id, Object::Dict(details_opts.clone())],
        )
        .unwrap();
        let Object::Array(fields) = with_details else { panic!("expected array") };
        assert_eq!(fields.len(), 3);
        let Object::Dict(d) = &fields[2] else { panic!("expected dict details") };
        assert_eq!(d.get("end_row"), Some(&Object::Integer(0)));
        assert_eq!(d.get("end_col"), Some(&Object::Integer(4)));
        assert_eq!(d.get("hl_group"), Some(&Object::str("DiagnosticError")));

        // get_extmarks over the whole buffer finds it too.
        let all = call(
            &mut cx,
            "nvim_buf_get_extmarks",
            &[buf, ns, Object::Integer(0), Object::Integer(-1), Object::Dict(details_opts)],
        )
        .unwrap();
        let Object::Array(rows) = all else { panic!("expected array") };
        assert_eq!(rows.len(), 1);
    }

    #[test]
    fn nvim_buf_clear_namespace_removes_marks_and_their_decorations() {
        let mut cx = ctx_with("a\nb\nc");
        let buf = call(&mut cx, "nvim_get_current_buf", &[]).unwrap();
        let ns = call(&mut cx, "nvim_create_namespace", &[Object::str("ns")]).unwrap();
        call(&mut cx, "nvim_buf_set_extmark", &[buf.clone(), ns.clone(), Object::Integer(0), Object::Integer(0), Object::Nil]).unwrap();
        call(&mut cx, "nvim_buf_set_extmark", &[buf.clone(), ns.clone(), Object::Integer(2), Object::Integer(0), Object::Nil]).unwrap();

        // Clear only line 0.
        call(&mut cx, "nvim_buf_clear_namespace", &[buf.clone(), ns.clone(), Object::Integer(0), Object::Integer(1)]).unwrap();
        let remaining = call(&mut cx, "nvim_buf_get_extmarks", &[buf.clone(), ns.clone(), Object::Integer(0), Object::Integer(-1), Object::Nil]).unwrap();
        let Object::Array(rows) = remaining else { panic!("expected array") };
        assert_eq!(rows.len(), 1, "only the mark on line 0 should be gone");

        // Clear the rest.
        call(&mut cx, "nvim_buf_clear_namespace", &[buf.clone(), ns.clone(), Object::Integer(0), Object::Integer(-1)]).unwrap();
        let remaining = call(&mut cx, "nvim_buf_get_extmarks", &[buf, ns, Object::Integer(0), Object::Integer(-1), Object::Nil]).unwrap();
        assert_eq!(remaining, Object::Array(Vec::new()));
    }

    #[test]
    fn nvim_buf_attach_watches_content_and_reports_a_changed_range() {
        let mut cx = ctx_with("a\nb\nc\nd");
        let buf = call(&mut cx, "nvim_get_current_buf", &[]).unwrap();
        let mut opts = std::collections::BTreeMap::new();
        opts.insert("on_lines".to_string(), Object::LuaRef(LuaRef(7)));
        let ok = call(&mut cx, "nvim_buf_attach", &[buf.clone(), Object::Boolean(false), Object::Dict(opts)]).unwrap();
        assert_eq!(ok, Object::Boolean(true));

        // No change yet.
        assert!(cx.check_buf_watcher(buf_id(&buf)).is_none());

        // Edit just line 1 (0-based) — the diff should isolate that line.
        call(
            &mut cx,
            "nvim_buf_set_lines",
            &[
                buf.clone(),
                Object::Integer(1),
                Object::Integer(2),
                Object::Boolean(false),
                Object::Array(vec![Object::str("B")]),
            ],
        )
        .unwrap();
        let (LuaRef(id), _tick, firstline, lastline, new_lastline) = cx.check_buf_watcher(buf_id(&buf)).unwrap();
        assert_eq!(id, 7);
        assert_eq!((firstline, lastline, new_lastline), (1, 2, 2));

        // Settles back to "no change" until the next edit.
        assert!(cx.check_buf_watcher(buf_id(&buf)).is_none());
    }

    #[test]
    fn nvim_buf_attach_without_on_lines_still_reports_success_but_watches_nothing() {
        let mut cx = ctx_with("x");
        let buf = call(&mut cx, "nvim_get_current_buf", &[]).unwrap();
        let ok = call(&mut cx, "nvim_buf_attach", &[buf.clone(), Object::Boolean(false), Object::Nil]).unwrap();
        assert_eq!(ok, Object::Boolean(true));
        call(
            &mut cx,
            "nvim_buf_set_lines",
            &[buf.clone(), Object::Integer(0), Object::Integer(1), Object::Boolean(false), Object::Array(vec![Object::str("y")])],
        )
        .unwrap();
        assert!(cx.check_buf_watcher(buf_id(&buf)).is_none());
    }

    #[test]
    fn nvim_exec_autocmds_is_not_registered_here() {
        // `nvim_exec_autocmds` needs to invoke Lua callbacks directly, which
        // this crate can't do (no `Lua`/registry access) — it's installed by
        // `ctrlvim-lua`'s `Host` instead. Documented here so its absence from
        // this crate's registry isn't mistaken for an oversight.
        assert!(registry::lookup("nvim_exec_autocmds").is_none());
    }
}
