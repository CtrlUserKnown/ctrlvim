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
    /// Keymaps registered from Lua (`vim.keymap.set`): (mode, lhs) -> LuaRef id.
    pub keymaps: HashMap<(String, String), i64>,
    /// Persistent Vimscript state (globals + user functions) backing `vim.fn`.
    pub script: ctrlvim_vimscript::ScriptState,
    namespaces: HashMap<String, u32>,
    next_namespace: u32,
}

impl ApiContext {
    pub fn new(editor: Editor) -> Self {
        ApiContext {
            session: Session::from_editor(editor),
            autocmds: AutocmdStore::new(),
            keymaps: HashMap::new(),
            script: ctrlvim_vimscript::ScriptState::default(),
            namespaces: HashMap::new(),
            next_namespace: 1,
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
}
