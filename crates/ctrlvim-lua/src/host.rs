//! The Lua host — `executor.c`'s job: own the Lua state, expose `vim.api` and
//! `vim.uv`, and invoke Lua callbacks (autocmds, timers) back from Rust.
//!
//! The editor state lives behind `Rc<RefCell<ApiContext>>` so it can be shared
//! into every `vim.api.*` closure. A call borrows it mutably only for the
//! duration of the dispatch, matching Neovim's single-threaded, one-call-at-a-
//! time execution model (the `textlock` guard exists in C for the same reason).

use crate::convert::{self, args_to_objects};
use crate::reg::LuaRefStore;
use mlua::{Function, Lua, MultiValue, Table, Value};
use ctrlvim_api::autocmd::CallbackRef;
use ctrlvim_api::ApiContext;
use ctrlvim_async::{Event, EventLoop, TimerHandle, TimerService};
use ctrlvim_editor::Editor;
use ctrlvim_types::object::LuaRef;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

/// A running Lua-enabled editor host.
pub struct Host {
    lua: Lua,
    ctx: Rc<RefCell<ApiContext>>,
    store: LuaRefStore,
    events: EventLoop,
    timers: Rc<RefCell<TimerService>>,
    /// timer id -> LuaRef id of its callback.
    timer_cbs: Rc<RefCell<HashMap<u64, i64>>>,
    /// keep timer handles alive so repeating timers keep firing / can be stopped.
    timer_handles: Rc<RefCell<HashMap<u64, TimerHandle>>>,
    /// Registered treesitter grammars (`vim.treesitter.language.add`).
    ts: Rc<RefCell<ctrlvim_treesitter::LanguageRegistry>>,
}

impl Host {
    /// Create a host over `editor`, installing the `vim.api` and `vim.uv`
    /// surfaces.
    pub fn new(editor: Editor) -> mlua::Result<Self> {
        let lua = Lua::new();
        let ctx = Rc::new(RefCell::new(ApiContext::new(editor)));
        let store = LuaRefStore::new();
        let events = EventLoop::new();
        let timers = TimerService::new(events.sender())
            .map_err(|e| mlua::Error::RuntimeError(e.to_string()))?;
        let host = Host {
            lua,
            ctx,
            store,
            events,
            timers: Rc::new(RefCell::new(timers)),
            timer_cbs: Rc::new(RefCell::new(HashMap::new())),
            timer_handles: Rc::new(RefCell::new(HashMap::new())),
            ts: Rc::new(RefCell::new(ctrlvim_treesitter::LanguageRegistry::new())),
        };
        host.install()?;
        Ok(host)
    }

    /// Register a treesitter grammar under a language name, so
    /// `vim.treesitter.query(name, ...)` can use it. The embedder supplies the
    /// grammar (`tree_sitter_json::LANGUAGE.into()`, etc.).
    pub fn register_ts_language(&self, name: &str, language: ctrlvim_treesitter::Language) {
        self.ts.borrow_mut().add(name, language);
    }

    /// Invoke a registered keymap callback (`vim.keymap.set`). A full typeahead/
    /// `:map` engine (M3 deferred) would call this during key dispatch; here it
    /// is the trigger the frontend/tests use.
    pub fn trigger_keymap(&self, mode: &str, lhs: &str) -> mlua::Result<bool> {
        let luaref = self
            .ctx
            .borrow()
            .keymaps
            .get(&(mode.to_string(), lhs.to_string()))
            .copied();
        match luaref {
            Some(id) => {
                if let Some(func) = self.store.get(&self.lua, id)? {
                    let _: () = func.call(())?;
                }
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// List plugin-registered commands (`vim.api.ctrlvim_create_user_command`):
    /// name, description, and source script (if any), for the frontend's
    /// command palette to display and attribute to a plugin.
    pub fn user_commands(&self) -> Vec<(String, String, Option<String>)> {
        self.ctx
            .borrow()
            .user_commands
            .iter()
            .map(|(name, (_, desc, source))| (name.clone(), desc.clone(), source.clone()))
            .collect()
    }

    /// Invoke a plugin-registered command by name (selecting it from the
    /// palette) — the same `LuaRef` invocation mechanism as keymaps/autocmds.
    /// Returns whether a command with that name was found.
    pub fn run_user_command(&self, name: &str) -> mlua::Result<bool> {
        let luaref = self.ctx.borrow().user_commands.get(name).map(|(r, _, _)| r.0);
        match luaref {
            Some(id) => {
                if let Some(func) = self.store.get(&self.lua, id)? {
                    let _: () = func.call(())?;
                }
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// Set (or clear) the script currently being sourced, so
    /// `ctrlvim_create_user_command` calls made while it runs are tagged with
    /// this source (see [`ApiContext::current_source`]).
    pub fn set_current_source(&self, source: Option<String>) {
        self.ctx.borrow_mut().current_source = source;
    }

    /// Build the global `vim` table with `api` and `uv` sub-tables.
    fn install(&self) -> mlua::Result<()> {
        let vim = self.lua.create_table()?;
        self.install_api(&vim)?;
        self.install_uv(&vim)?;
        self.install_fn(&vim)?;
        self.install_keymap(&vim)?;
        self.install_treesitter(&vim)?;
        vim.set("NIL", Value::Nil)?; // `vim.NIL` sentinel
        self.lua.globals().set("vim", vim)?;
        Ok(())
    }

    /// Install `vim.keymap.set(mode, lhs, callback)` — stores the callback as a
    /// `LuaRef` in the shared keymap table (same mechanism as autocmds).
    fn install_keymap(&self, vim: &Table) -> mlua::Result<()> {
        let keymap = self.lua.create_table()?;
        let ctx = self.ctx.clone();
        let store = self.store.clone();
        let set = self.lua.create_function(
            move |lua, (mode, lhs, cb): (String, String, Function)| {
                let id = store.store(lua, cb)?;
                ctx.borrow_mut().keymaps.insert((mode, lhs), id);
                Ok(())
            },
        )?;
        keymap.set("set", set)?;
        vim.set("keymap", keymap)?;
        Ok(())
    }

    /// Install `vim.treesitter` — parse/query over registered grammars. Returns
    /// captures as plain tables (row/col/text), the shape a highlighter wants.
    fn install_treesitter(&self, vim: &Table) -> mlua::Result<()> {
        let tst = self.lua.create_table()?;

        let ts = self.ts.clone();
        let query = self.lua.create_function(
            move |lua, (lang, source, query): (String, String, String)| {
                let reg = ts.borrow();
                let tree = reg
                    .parse(&lang, &source)
                    .ok_or_else(|| mlua::Error::RuntimeError(format!("no parser for language '{lang}'")))?;
                let language = reg.get(&lang).unwrap();
                let caps = tree.query(language, &query).map_err(mlua::Error::RuntimeError)?;
                let arr = lua.create_table()?;
                for (i, c) in caps.iter().enumerate() {
                    let t = lua.create_table()?;
                    t.set("name", c.name.clone())?;
                    t.set("kind", c.kind.clone())?;
                    t.set("start_row", c.start.0)?;
                    t.set("start_col", c.start.1)?;
                    t.set("end_row", c.end.0)?;
                    t.set("end_col", c.end.1)?;
                    t.set("text", c.text.clone())?;
                    arr.set(i + 1, t)?;
                }
                Ok(arr)
            },
        )?;
        tst.set("query", query)?;

        let ts2 = self.ts.clone();
        let root_kind = self.lua.create_function(move |_lua, (lang, source): (String, String)| {
            let reg = ts2.borrow();
            let tree = reg
                .parse(&lang, &source)
                .ok_or_else(|| mlua::Error::RuntimeError(format!("no parser for language '{lang}'")))?;
            Ok(tree.root_kind())
        })?;
        tst.set("root_kind", root_kind)?;

        vim.set("treesitter", tst)?;
        Ok(())
    }

    /// Install `vim.fn` and `vim.call` — the Vimscript builtin/user-function
    /// surface, routed through the persistent interpreter in `ApiContext`.
    fn install_fn(&self, vim: &Table) -> mlua::Result<()> {
        // vim.call(name, ...args)
        let ctx = self.ctx.clone();
        let store = self.store.clone();
        let call = self.lua.create_function(move |lua, mut args: MultiValue| {
            let name = match args.pop_front() {
                Some(Value::String(s)) => s.to_str()?.to_string(),
                _ => return Err(mlua::Error::RuntimeError("vim.call: first arg must be a function name".into())),
            };
            let objs = args_to_objects(lua, &args, &store)?;
            let result = {
                let mut c = ctx.borrow_mut();
                c.call_vimfn(&name, objs)
            };
            match result {
                Ok(o) => convert::to_lua(lua, &o, &store),
                Err(e) => Err(mlua::Error::RuntimeError(e.to_string())),
            }
        })?;
        vim.set("call", call)?;

        // vim.fn.<name>(...) via an __index metatable returning a bound closure.
        let fn_table = self.lua.create_table()?;
        let meta = self.lua.create_table()?;
        let ctx2 = self.ctx.clone();
        let store2 = self.store.clone();
        let index = self.lua.create_function(move |lua, (_t, key): (Table, String)| {
            let ctx = ctx2.clone();
            let store = store2.clone();
            let name = key;
            lua.create_function(move |lua, args: MultiValue| {
                let objs = args_to_objects(lua, &args, &store)?;
                let result = {
                    let mut c = ctx.borrow_mut();
                    c.call_vimfn(&name, objs)
                };
                match result {
                    Ok(o) => convert::to_lua(lua, &o, &store),
                    Err(e) => Err(mlua::Error::RuntimeError(e.to_string())),
                }
            })
        })?;
        meta.set("__index", index)?;
        fn_table.set_metatable(Some(meta));
        vim.set("fn", fn_table)?;
        Ok(())
    }

    /// Run a Vimscript chunk (`vim.cmd`-style source).
    pub fn exec_vimscript(&self, src: &str) -> mlua::Result<()> {
        self.ctx
            .borrow_mut()
            .exec_vimscript(src)
            .map_err(|e| mlua::Error::RuntimeError(e.to_string()))
    }

    /// Install `vim.api` — the Rust equivalent of `nlua_add_api_functions`.
    fn install_api(&self, vim: &Table) -> mlua::Result<()> {
        let api = self.lua.create_table()?;
        for f in ctrlvim_api::registry::all() {
            let name = f.name;
            let ctx = self.ctx.clone();
            let store = self.store.clone();
            let func = self.lua.create_function(move |lua, args: MultiValue| {
                let objs = args_to_objects(lua, &args, &store)?;
                let result = {
                    let mut c = ctx.borrow_mut();
                    ctrlvim_api::registry::call(&mut c, name, &objs)
                };
                match result {
                    Ok(obj) => convert::to_lua(lua, &obj, &store),
                    Err(e) => Err(mlua::Error::RuntimeError(e.to_string())),
                }
            })?;
            api.set(name, func)?;
        }
        vim.set("api", api)?;
        Ok(())
    }

    /// Install `vim.uv` (and its `vim.loop` alias) — the tokio-backed timer
    /// binding. A `new_timer()` returns a handle table with `start`/`stop`
    /// methods, mirroring luv's `uv_timer_t`.
    fn install_uv(&self, vim: &Table) -> mlua::Result<()> {
        let uv = self.lua.create_table()?;

        let store = self.store.clone();
        let timers = self.timers.clone();
        let timer_cbs = self.timer_cbs.clone();
        let timer_handles = self.timer_handles.clone();

        let new_timer = self.lua.create_function(move |lua, ()| {
            let handle = lua.create_table()?;

            // `timer:start(timeout, repeat, callback)`
            let s_store = store.clone();
            let s_timers = timers.clone();
            let s_cbs = timer_cbs.clone();
            let s_handles = timer_handles.clone();
            let start = lua.create_function(
                move |lua, (_self, timeout, repeat, cb): (Table, u64, u64, Function)| {
                    let luaref = s_store.store(lua, cb)?;
                    let th = s_timers
                        .borrow_mut()
                        .start(Duration::from_millis(timeout), Duration::from_millis(repeat));
                    let tid = th.id;
                    s_cbs.borrow_mut().insert(tid, luaref);
                    s_handles.borrow_mut().insert(tid, th);
                    Ok(())
                },
            )?;
            handle.set("start", start)?;

            // `timer:stop()`
            let st_handles = timer_handles.clone();
            let st_cbs = timer_cbs.clone();
            let stop = lua.create_function(move |_lua, _self: Table| {
                // Stop every timer this handle owns. (A 1:1 handle→timer model
                // would track the id on the handle; we stop all for simplicity.)
                for (_id, h) in st_handles.borrow().iter() {
                    h.stop();
                }
                st_handles.borrow_mut().clear();
                st_cbs.borrow_mut().clear();
                Ok(())
            })?;
            handle.set("stop", stop)?;

            Ok(handle)
        })?;

        uv.set("new_timer", new_timer)?;
        vim.set("uv", uv.clone())?;
        vim.set("loop", uv)?; // legacy alias
        Ok(())
    }

    /// Execute a chunk of Lua (`:lua ...`).
    pub fn exec(&self, code: &str) -> mlua::Result<()> {
        self.lua.load(code).exec()
    }

    /// Evaluate a Lua expression and return it as a string (for demos/tests).
    pub fn eval_string(&self, expr: &str) -> mlua::Result<String> {
        let v: Value = self.lua.load(expr).eval()?;
        Ok(match v {
            Value::String(s) => s.to_str()?.to_string(),
            Value::Integer(i) => i.to_string(),
            Value::Number(n) => n.to_string(),
            Value::Boolean(b) => b.to_string(),
            Value::Nil => "nil".to_string(),
            other => format!("{other:?}"),
        })
    }

    /// Fire an autocommand event, invoking every matching Lua callback — the
    /// `apply_autocmds`/`au_callback` path. Reuses the one `LuaRef` invocation
    /// mechanism (`nlua_call_ref`).
    pub fn fire_autocmd(&self, event: &str, file: &str) -> mlua::Result<()> {
        let callbacks = self.ctx.borrow_mut().autocmds.fire(event, file);
        for cb in callbacks {
            match cb {
                CallbackRef::Lua(LuaRef(id)) => {
                    if let Some(func) = self.store.get(&self.lua, id)? {
                        let arg = self.lua.create_table()?;
                        arg.set("event", event)?;
                        arg.set("file", file)?;
                        arg.set("match", file)?;
                        let _: () = func.call(arg)?;
                    }
                }
                CallbackRef::Command(_cmd) => {
                    // Ex-command autocmds require the Vimscript executor (M6).
                }
            }
        }
        Ok(())
    }

    /// Drive the event loop for up to `timeout`, invoking Lua callbacks for any
    /// timers that fire. Returns the number of callbacks invoked. This is the
    /// model of `state_enter`'s wait-then-process-`K_EVENT` cycle: waiting
    /// happens on tokio, callback invocation happens here on the editor thread.
    pub fn run_events(&self, timeout: Duration) -> mlua::Result<usize> {
        let mut invoked = 0;
        // Block for the first event, then drain whatever else is ready.
        let mut batch = Vec::new();
        if let Some(ev) = self.events.wait(timeout) {
            batch.push(ev);
            batch.extend(self.events.drain());
        }
        for ev in batch {
            if let Event::TimerFired(tid) = ev {
                let luaref = self.timer_cbs.borrow().get(&tid).copied();
                if let Some(id) = luaref {
                    if let Some(func) = self.store.get(&self.lua, id)? {
                        let _: () = func.call(())?;
                        invoked += 1;
                    }
                }
            }
        }
        Ok(invoked)
    }

    /// Borrow the editor for inspection (tests/frontend).
    pub fn with_editor<T>(&self, f: impl FnOnce(&Editor) -> T) -> T {
        f(self.ctx.borrow().editor())
    }

    /// Mutably borrow the editor (frontend feeds keys, etc.).
    pub fn with_editor_mut<T>(&self, f: impl FnOnce(&mut Editor) -> T) -> T {
        f(self.ctx.borrow_mut().editor_mut())
    }

    /// Feed one key through the shared modal session — interactive input and
    /// Lua now act on the *same* editor.
    pub fn feed_key(&self, key: ctrlvim_editor::Key) {
        self.ctx.borrow_mut().session.feed(key);
    }

    /// Feed a `<...>`-encoded key sequence.
    pub fn feed_keys(&self, seq: &str) {
        let keys = ctrlvim_editor::Key::parse_sequence(seq);
        let mut c = self.ctx.borrow_mut();
        for k in keys {
            c.session.feed(k);
        }
    }

    /// Handle one incoming msgpack-RPC message, dispatching through the same
    /// `ctrlvim-api` registry the Lua binding uses. Returns response bytes for a
    /// request. This is the RPC half of the channel layer.
    pub fn handle_rpc(&self, bytes: &[u8]) -> Result<Option<Vec<u8>>, String> {
        let ctx = self.ctx.clone();
        ctrlvim_async::rpc::handle(bytes, move |method, params| {
            let mut c = ctx.borrow_mut();
            ctrlvim_api::registry::call(&mut c, method, &params)
        })
    }

    /// Number of live Lua callbacks held (for leak checks in tests).
    pub fn callback_count(&self) -> usize {
        self.store.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host_with(text: &str) -> Host {
        let mut ed = Editor::new();
        ed.load_str(text, None);
        Host::new(ed).unwrap()
    }

    #[test]
    fn lua_can_call_vim_api_get_current_line() {
        let host = host_with("hello from lua");
        let out = host.eval_string("vim.api.ctrlvim_get_current_line()").unwrap();
        assert_eq!(out, "hello from lua");
    }

    #[test]
    fn lua_can_mutate_buffer_through_api() {
        let host = host_with("old line");
        host.exec("vim.api.ctrlvim_set_current_line('new line')").unwrap();
        let out = host.eval_string("vim.api.ctrlvim_get_current_line()").unwrap();
        assert_eq!(out, "new line");
        host.with_editor(|ed| {
            assert_eq!(ed.cur_buffer().text.line(0).as_deref(), Some("new line"));
        });
    }

    #[test]
    fn lua_win_cursor_roundtrip() {
        let host = host_with("line one\nline two\nline three");
        host.exec("vim.api.ctrlvim_win_set_cursor({3, 2})").unwrap();
        let row = host.eval_string("vim.api.ctrlvim_win_get_cursor()[1]").unwrap();
        let col = host.eval_string("vim.api.ctrlvim_win_get_cursor()[2]").unwrap();
        assert_eq!((row.as_str(), col.as_str()), ("3", "2"));
    }

    #[test]
    fn lua_autocmd_callback_fires() {
        let host = host_with("data");
        host.exec(
            r#"
            _G.fired = false
            _G.got_file = nil
            vim.api.ctrlvim_create_autocmd('BufWritePre', {
                pattern = '*',
                callback = function(ev)
                    _G.fired = true
                    _G.got_file = ev.file
                end,
            })
            "#,
        )
        .unwrap();
        assert_eq!(host.eval_string("tostring(_G.fired)").unwrap(), "false");
        assert_eq!(host.callback_count(), 1);
        host.fire_autocmd("BufWritePre", "example.txt").unwrap();
        assert_eq!(host.eval_string("tostring(_G.fired)").unwrap(), "true");
        assert_eq!(host.eval_string("_G.got_file").unwrap(), "example.txt");
    }

    #[test]
    fn type_error_from_api_becomes_lua_error() {
        let host = host_with("x");
        let err = host.exec("vim.api.ctrlvim_set_current_line(42)").unwrap_err();
        assert!(err.to_string().contains("expected string"));
    }

    #[test]
    fn table_arg_converts_to_dict() {
        let host = host_with("x");
        host.exec(
            r#"
            vim.api.ctrlvim_create_autocmd('BufEnter', {
                pattern = '*.rs',
                once = true,
                callback = function() _G.count = (_G.count or 0) + 1 end,
            })
            "#,
        )
        .unwrap();
        host.fire_autocmd("BufEnter", "main.rs").unwrap();
        host.fire_autocmd("BufEnter", "main.rs").unwrap();
        assert_eq!(host.eval_string("tostring(_G.count)").unwrap(), "1");
    }

    #[test]
    fn vim_uv_timer_fires_lua_callback() {
        let host = host_with("x");
        // Unmodified luv-style timer usage.
        host.exec(
            r#"
            _G.ticked = false
            local t = vim.uv.new_timer()
            t:start(10, 0, function() _G.ticked = true end)
            "#,
        )
        .unwrap();
        assert_eq!(host.eval_string("tostring(_G.ticked)").unwrap(), "false");
        let invoked = host.run_events(Duration::from_secs(2)).unwrap();
        assert_eq!(invoked, 1);
        assert_eq!(host.eval_string("tostring(_G.ticked)").unwrap(), "true");
    }

    #[test]
    fn vim_loop_is_an_alias_for_vim_uv() {
        let host = host_with("x");
        // `vim.loop` should resolve (the legacy name many plugins still use).
        assert_eq!(
            host.eval_string("tostring(vim.loop.new_timer ~= nil)").unwrap(),
            "true"
        );
    }

    #[test]
    fn vim_fn_builtin_from_lua() {
        let host = host_with("x");
        // vim.fn.len / vim.fn.range / vim.fn.join through the metatable proxy.
        assert_eq!(host.eval_string("vim.fn.len({1, 2, 3, 4})").unwrap(), "4");
        assert_eq!(host.eval_string("vim.fn.join({'a', 'b', 'c'}, '-')").unwrap(), "a-b-c");
        assert_eq!(host.eval_string("vim.fn.toupper('hello')").unwrap(), "HELLO");
    }

    #[test]
    fn vim_fn_getline_sees_editor() {
        let host = host_with("first line\nsecond line");
        // getline(1) reads the shared editor buffer.
        assert_eq!(host.eval_string("vim.fn.getline(1)").unwrap(), "first line");
        assert_eq!(host.eval_string("vim.fn.line('$')").unwrap(), "2");
    }

    #[test]
    fn vimscript_user_function_callable_from_lua() {
        let host = host_with("x");
        host.exec_vimscript("function! Double(n)\n  return a:n * 2\nendfunction").unwrap();
        assert_eq!(host.eval_string("vim.fn.Double(21)").unwrap(), "42");
    }

    #[test]
    fn vim_treesitter_query_from_lua() {
        let host = host_with("x");
        host.register_ts_language("json", tree_sitter_json::LANGUAGE.into());
        host.exec(
            r#"
            local caps = vim.treesitter.query('json', '{"n": 42}', '(number) @num')
            _G.num_text = caps[1].text
            _G.num_kind = caps[1].kind
            "#,
        )
        .unwrap();
        assert_eq!(host.eval_string("_G.num_text").unwrap(), "42");
        assert_eq!(host.eval_string("_G.num_kind").unwrap(), "number");
    }

    #[test]
    fn vim_keymap_set_and_trigger() {
        let host = host_with("x");
        host.exec(
            r#"
            _G.pressed = 0
            vim.keymap.set('n', '<leader>x', function() _G.pressed = _G.pressed + 1 end)
            "#,
        )
        .unwrap();
        assert_eq!(host.eval_string("tostring(_G.pressed)").unwrap(), "0");
        assert!(host.trigger_keymap("n", "<leader>x").unwrap());
        assert_eq!(host.eval_string("tostring(_G.pressed)").unwrap(), "1");
    }

    #[test]
    fn plugin_registered_command_is_listed_and_runnable() {
        let host = host_with("x");
        host.exec(
            r#"
            _G.ran = 0
            vim.api.ctrlvim_create_user_command('Greet', function()
                _G.ran = _G.ran + 1
            end, { desc = 'say hello' })
            "#,
        )
        .unwrap();
        assert_eq!(host.user_commands(), vec![("Greet".to_string(), "say hello".to_string(), None)]);
        assert!(host.run_user_command("Greet").unwrap());
        assert_eq!(host.eval_string("tostring(_G.ran)").unwrap(), "1");
        assert!(!host.run_user_command("NoSuchCommand").unwrap());
    }

    #[test]
    fn plugin_registered_command_is_tagged_with_its_source() {
        let host = host_with("x");
        host.set_current_source(Some("my-plugin".to_string()));
        host.exec(
            r#"
            vim.api.ctrlvim_create_user_command('Greet', function() end, {})
            "#,
        )
        .unwrap();
        host.set_current_source(None);
        assert_eq!(
            host.user_commands(),
            vec![("Greet".to_string(), String::new(), Some("my-plugin".to_string()))]
        );
    }

    /// M8 — a representative "unmodified-style" plugin that leans on api + fn +
    /// autocmd + keymap + treesitter together, loaded and driven end-to-end.
    #[test]
    fn plugin_integration_end_to_end() {
        let host = host_with("{\"greeting\": \"hi\", \"count\": 3}");
        host.register_ts_language("json", tree_sitter_json::LANGUAGE.into());

        // A plugin's init: it registers an autocmd + a keymap, and defines a
        // command function that inspects the buffer via api/fn/treesitter.
        host.exec(
            r#"
            local M = {}

            -- count JSON string nodes in the current buffer using treesitter
            function M.count_strings()
              local src = vim.api.ctrlvim_get_current_line()
              local caps = vim.treesitter.query('json', src, '(string) @s')
              return #caps
            end

            -- expose state a test can read
            _G.plugin = M
            _G.events = {}

            vim.api.ctrlvim_create_autocmd('BufWritePre', {
              pattern = '*',
              callback = function(ev)
                table.insert(_G.events, 'save:' .. ev.file)
              end,
            })

            vim.keymap.set('n', '<leader>c', function()
              _G.last_count = M.count_strings()
            end)
            "#,
        )
        .unwrap();

        // Interactive key input flows through the SAME editor the Lua sees.
        host.feed_keys("$");
        assert_eq!(host.eval_string("vim.fn.line('$')").unwrap(), "1");

        // Trigger the plugin's keymap: it runs a treesitter query over the buffer.
        assert!(host.trigger_keymap("n", "<leader>c").unwrap());
        // JSON: keys "greeting"/"count" + value "hi" = 3 string nodes.
        assert_eq!(host.eval_string("tostring(_G.last_count)").unwrap(), "3");

        // Fire the plugin's autocmd; it appends to a Lua-side log.
        host.fire_autocmd("BufWritePre", "config.json").unwrap();
        assert_eq!(host.eval_string("_G.events[1]").unwrap(), "save:config.json");
    }

    #[test]
    fn rpc_request_dispatches_through_same_registry() {
        let host = host_with("hello rpc");
        let req = ctrlvim_async::rpc::encode(&ctrlvim_async::rpc::Message::Request {
            msgid: 1,
            method: "ctrlvim_get_current_line".into(),
            params: vec![],
        });
        let resp = host.handle_rpc(&req).unwrap().unwrap();
        match ctrlvim_async::rpc::decode(&resp).unwrap() {
            ctrlvim_async::rpc::Message::Response { result, .. } => {
                assert_eq!(result.as_str(), Some("hello rpc"));
            }
            _ => panic!("expected response"),
        }
    }
}
