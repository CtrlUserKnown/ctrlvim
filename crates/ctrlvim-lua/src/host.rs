//! The Lua host — `executor.c`'s job: own the Lua state, expose `vim.api` and
//! `vim.uv`, and invoke Lua callbacks (autocmds, timers) back from Rust.
//!
//! The editor state lives behind `Rc<RefCell<ApiContext>>` so it can be shared
//! into every `vim.api.*` closure. A call borrows it mutably only for the
//! duration of the dispatch, matching Neovim's single-threaded, one-call-at-a-
//! time execution model (the `textlock` guard exists in C for the same reason).

use crate::convert::{self, args_to_objects};
use crate::reg::LuaRefStore;
use mlua::{Function, Lua, LuaSerdeExt, MultiValue, RegistryKey, Table, Value};
use ctrlvim_api::autocmd::CallbackRef;
use ctrlvim_api::ApiContext;
use ctrlvim_async::{Event, EventLoop, JobStdin, Jobs, TimerHandle, TimerService};
use ctrlvim_editor::Editor;
use ctrlvim_types::object::LuaRef;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
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
    /// Messages queued by `vim.notify`, drained by the frontend.
    notices: Rc<RefCell<Vec<(i64, String)>>>,
    /// LuaRef ids queued by `vim.schedule`, run at the next safe point.
    scheduled: Rc<RefCell<Vec<i64>>>,
    /// `require()` search roots — each contributes `<root>/lua/?.lua` and
    /// `<root>/lua/?/init.lua`, mirroring Neovim's runtimepath. Populated by
    /// the embedder via [`Host::add_runtime_path`] for every plugin directory
    /// (`[[plugin]]` entries and `pack/*/start/*`), so a multi-file plugin's
    /// internal `require()` calls resolve the same way they would in Neovim.
    runtime_paths: Rc<RefCell<Vec<PathBuf>>>,
    /// Backs `vim.uv.spawn` — shares `timers`' tokio runtime and `events`'
    /// queue, same as every other background source in this file.
    jobs: Rc<RefCell<Jobs>>,
    /// `uv.new_pipe()` handle id -> what it's bound to, set once the pipe is
    /// used in a `uv.spawn` `stdio` table. A pipe with no entry yet is
    /// unbound: `:read_start`/`:write` on it are no-ops, matching the fact
    /// that real code never touches a pipe before spawning with it.
    pipe_roles: Rc<RefCell<HashMap<u64, PipeRole>>>,
    /// Pipe id -> its `:read_start(callback)` registration.
    pipe_read_cbs: Rc<RefCell<HashMap<u64, Rc<RegistryKey>>>>,
    /// Job id -> its writable stdin, for `pipe:write()` on the pipe bound as
    /// that job's `PipeRole::Stdin`.
    job_stdin: Rc<RefCell<HashMap<u64, JobStdin>>>,
    /// Job id -> its `uv.spawn(..., on_exit)` callback.
    job_on_exit: Rc<RefCell<HashMap<u64, Rc<RegistryKey>>>>,
    next_pipe_id: Rc<RefCell<u64>>,
}

/// What a `uv.new_pipe()` handle was bound to by a `uv.spawn` call — see
/// [`Host::pipe_roles`].
#[derive(Clone, Copy)]
enum PipeRole {
    Stdin { job_id: u64 },
    Stdout { job_id: u64 },
    Stderr { job_id: u64 },
}

impl PipeRole {
    fn job_id(self) -> u64 {
        match self {
            PipeRole::Stdin { job_id } | PipeRole::Stdout { job_id } | PipeRole::Stderr { job_id } => job_id,
        }
    }
}

impl Host {
    /// Create a host over `editor`, installing the `vim.api` and `vim.uv`
    /// surfaces.
    pub fn new(editor: Editor) -> mlua::Result<Self> {
        // `ALL_SAFE` alone excludes the `debug` library; real Neovim exposes
        // it (unsandboxed Lua does too), and vendored runtime code
        // (`vim.lsp.log`'s trace-level logging) reaches for
        // `debug.traceback`. Nothing else here needs sandboxing from a
        // user's own config/plugins in the way a server embedding untrusted
        // scripts would — this host already gives config/plugins full
        // `vim.uv`/process-spawn access, so `debug` isn't a new trust
        // boundary crossed.
        let lua = unsafe { Lua::unsafe_new_with(mlua::StdLib::ALL_SAFE | mlua::StdLib::DEBUG, mlua::LuaOptions::default()) };
        let ctx = Rc::new(RefCell::new(ApiContext::new(editor)));
        let store = LuaRefStore::new();
        let events = EventLoop::new();
        let timers = TimerService::new(events.sender())
            .map_err(|e| mlua::Error::RuntimeError(e.to_string()))?;
        let jobs = Jobs::new(timers.runtime().handle().clone(), events.sender());
        let host = Host {
            lua,
            ctx,
            store,
            events,
            timers: Rc::new(RefCell::new(timers)),
            timer_cbs: Rc::new(RefCell::new(HashMap::new())),
            timer_handles: Rc::new(RefCell::new(HashMap::new())),
            ts: Rc::new(RefCell::new(ctrlvim_treesitter::LanguageRegistry::new())),
            notices: Rc::new(RefCell::new(Vec::new())),
            scheduled: Rc::new(RefCell::new(Vec::new())),
            runtime_paths: Rc::new(RefCell::new(Vec::new())),
            jobs: Rc::new(RefCell::new(jobs)),
            pipe_roles: Rc::new(RefCell::new(HashMap::new())),
            pipe_read_cbs: Rc::new(RefCell::new(HashMap::new())),
            job_stdin: Rc::new(RefCell::new(HashMap::new())),
            job_on_exit: Rc::new(RefCell::new(HashMap::new())),
            next_pipe_id: Rc::new(RefCell::new(1)),
        };
        host.install()?;
        Ok(host)
    }

    /// Add a directory to the `require()` search path: `<dir>/lua/?.lua` and
    /// `<dir>/lua/?/init.lua` become resolvable module roots, same as adding
    /// `dir` to Neovim's `'runtimepath'`. Idempotent — adding the same
    /// directory twice (e.g. a plugin declared in config *and* found under a
    /// pack directory) only searches it once.
    pub fn add_runtime_path(&self, dir: impl Into<PathBuf>) {
        let dir = dir.into();
        let mut roots = self.runtime_paths.borrow_mut();
        if !roots.contains(&dir) {
            roots.push(dir);
        }
    }

    /// Load the vendored `vim.lsp`/`vim.diagnostic` runtime (see
    /// `runtime/NOTICE.md`) and wire it onto the global `vim` table, the same
    /// end state real Neovim's C side produces at startup.
    ///
    /// Not called automatically by [`Host::new`] — loading ~550KB of real
    /// Neovim Lua on every Host (including the many created in this crate's
    /// own tests) would be wasteful for a host that never touches LSP, and
    /// the coverage gap against the ~150 additional `vim.*` symbols these
    /// files reach for (documented in `runtime/NOTICE.md`) makes "always
    /// load, ignore errors" the wrong default. The embedder calls this once,
    /// when it actually wants LSP available.
    pub fn load_vendored_lsp_runtime(&self) -> mlua::Result<()> {
        self.lua.load(crate::vendored::BOOTSTRAP).set_name("@ctrlvim:bootstrap").exec()
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
            .map(|(id, _)| *id);
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
        self.install_opt(&vim)?;
        self.install_vars(&vim)?;
        self.install_cmd(&vim)?;
        self.install_stdlib(&vim)?;
        self.install_fs(&vim)?;
        self.install_lua51_compat()?;
        vim.set("NIL", Value::Nil)?; // `vim.NIL` sentinel
        self.lua.globals().set("vim", vim)?;
        self.install_require()?;

        // `vim.schedule_wrap(fn)` — expressible purely in terms of
        // `vim.schedule` (just installed above), so it's plain Lua rather
        // than more Rust glue. Must run after `vim` is a real global.
        self.lua
            .load(
                r#"
                function vim.schedule_wrap(fn)
                  return function(...)
                    local args, n = { ... }, select('#', ...)
                    vim.schedule(function()
                      fn(unpack(args, 1, n))
                    end)
                  end
                end
                "#,
            )
            .set_name("@ctrlvim:schedule_wrap")
            .exec()?;

        // `vim.system(cmd, opts, on_exit)` — real Neovim's is ~500 lines in
        // `vim/_core/system.lua`, built on a `uv.new_check()` polling handle
        // this engine doesn't implement. Rather than vendor that (and its
        // `vim.wait`/synchronous-`:wait()` machinery this codebase has no
        // use for yet), this is a from-scratch implementation of exactly the
        // subset `vim.lsp.rpc`'s transport actually calls: async spawn with
        // stdin/stdout/stderr callbacks and an `on_exit` handler — built on
        // the same tested `vim.uv.spawn`/`new_pipe` primitives above, not
        // hand-rolled Rust. `SystemObj:wait()` is a documented gap (errors
        // rather than blocking) since nothing in the LSP path calls it.
        self.lua
            .load(
                r#"
                function vim.system(cmd, opts, on_exit)
                  opts = opts or {}
                  local stdin_pipe = opts.stdin and vim.uv.new_pipe(false) or nil
                  local stdout_pipe = vim.uv.new_pipe(false)
                  local stderr_pipe = vim.uv.new_pipe(false)
                  local stdout_chunks, stderr_chunks = {}, {}

                  local args = {}
                  for i = 2, #cmd do args[#args + 1] = cmd[i] end

                  local obj = {}
                  local handle = vim.uv.spawn(cmd[1], {
                    args = args,
                    cwd = opts.cwd,
                    stdio = { stdin_pipe, stdout_pipe, stderr_pipe },
                  }, function(code, signal)
                    obj.code = code
                    obj.signal = signal
                    if #stdout_chunks > 0 then obj.stdout = table.concat(stdout_chunks) end
                    if #stderr_chunks > 0 then obj.stderr = table.concat(stderr_chunks) end
                    if on_exit then on_exit(obj) end
                  end)

                  stdout_pipe:read_start(function(err, data)
                    if type(opts.stdout) == 'function' then
                      opts.stdout(err, data)
                    elseif opts.stdout == true and data then
                      table.insert(stdout_chunks, data)
                    end
                  end)
                  stderr_pipe:read_start(function(err, data)
                    if type(opts.stderr) == 'function' then
                      opts.stderr(err, data)
                    elseif opts.stderr == true and data then
                      table.insert(stderr_chunks, data)
                    end
                  end)

                  function obj:write(data)
                    if not stdin_pipe then
                      return
                    elseif data == nil then
                      stdin_pipe:close()
                    else
                      stdin_pipe:write(data)
                    end
                  end
                  function obj:kill(signal)
                    handle:kill(signal)
                  end
                  function obj:is_closing()
                    return handle:is_closing()
                  end
                  function obj:wait(_timeout)
                    error('vim.system(...):wait() is not implemented in ctrlvim yet -- use the on_exit callback')
                  end

                  return obj
                end
                "#,
            )
            .set_name("@ctrlvim:system")
            .exec()?;

        // `vim.b`/`vim.bo` (buffer-scoped) and `vim.w`/`vim.wo`
        // (window-scoped) — plain per-handle tables. Real Neovim backs
        // `bo`/`wo` with actual option storage (a handful of which — tabstop/
        // shiftwidth/expandtab/number/wrap — this engine already models for
        // real via `vim.opt`, currently global-only); everything else here
        // (`filetype`, `buftype`, `modifiable`, ...) is settable/gettable but
        // not wired to editor behavior beyond that, same posture as most of
        // this compatibility layer: real storage, not yet acted upon.
        self.lua
            .load(
                r#"
                local function scoped(current_fn)
                  local store = {}
                  return setmetatable({}, {
                    __index = function(_, k)
                      if type(k) == 'number' then
                        store[k] = store[k] or {}
                        return store[k]
                      end
                      local id = current_fn()
                      store[id] = store[id] or {}
                      return store[id][k]
                    end,
                    __newindex = function(_, k, v)
                      local id = current_fn()
                      store[id] = store[id] or {}
                      store[id][k] = v
                    end,
                  })
                end
                vim.b = scoped(vim.api.nvim_get_current_buf)
                vim.bo = scoped(vim.api.nvim_get_current_buf)
                vim.w = scoped(vim.api.nvim_get_current_win)
                vim.wo = scoped(vim.api.nvim_get_current_win)
                "#,
            )
            .set_name("@ctrlvim:scoped_vars")
            .exec()?;

        Ok(())
    }

    /// Install a Neovim-runtimepath-shaped `require()` and its backing
    /// `package.loaded`/`package.preload` tables, replacing Lua's own stdlib
    /// `require` (which only knows the system Lua path — useless for a
    /// plugin's `require('lspconfig.util')`-style internal requires).
    ///
    /// Search order per call: `package.preload[name]` (an embedder can seed a
    /// module without a file, same as Neovim's own C modules do), then each
    /// [`Self::add_runtime_path`]-registered root's `lua/<name-with-slashes>.lua`
    /// then `.../init.lua`. A module is executed at most once; its result (or
    /// `true` if it returned nothing) is cached in `package.loaded`, and a
    /// module that `require`s itself transitively sees `true` rather than
    /// recursing forever, matching the real `require`'s loading-in-progress
    /// sentinel.
    fn install_require(&self) -> mlua::Result<()> {
        let lua = &self.lua;
        let loaded = lua.create_table()?;
        let preload = lua.create_table()?;
        let package = lua.create_table()?;
        package.set("loaded", loaded.clone())?;
        package.set("preload", preload.clone())?;
        lua.globals().set("package", package)?;

        // Seed `package.preload` with Neovim's own vendored `vim.lsp`/
        // `vim.diagnostic` source (see `vendored::MODULES`) — embedded in the
        // binary, so `require('vim.lsp')` resolves without touching disk.
        for (name, src) in crate::vendored::MODULES {
            let src = *src;
            let chunk_name = format!("@vendor:{name}");
            let loader = lua.create_function(move |lua, modname: String| {
                lua.load(src).set_name(chunk_name.clone()).call::<_, Value>(modname)
            })?;
            preload.set(*name, loader)?;
        }

        // `Table<'lua>` can't be captured by a `'static` closure (mlua ties its
        // lifetime to this call's `&self.lua` borrow) — stash both tables in the
        // registry instead and re-fetch them each call through the closure's own
        // `lua` parameter, which has a fresh, valid lifetime every invocation.
        let loaded_key = Rc::new(lua.create_registry_value(loaded)?);
        let preload_key = Rc::new(lua.create_registry_value(preload)?);
        let roots = self.runtime_paths.clone();
        let require = lua.create_function(move |lua, name: String| {
            let loaded: Table = lua.registry_value(&loaded_key)?;
            let preload: Table = lua.registry_value(&preload_key)?;

            let cached = loaded.get::<_, Value>(name.as_str())?;
            if !matches!(cached, Value::Nil) {
                return Ok(cached);
            }

            let preloader = preload.get::<_, Value>(name.as_str())?;
            let value = if let Value::Function(f) = preloader {
                f.call::<_, Value>(name.as_str())?
            } else {
                let rel = name.replace('.', "/");
                let path = roots
                    .borrow()
                    .iter()
                    .find_map(|root| {
                        let flat = root.join("lua").join(format!("{rel}.lua"));
                        let nested = root.join("lua").join(&rel).join("init.lua");
                        if flat.is_file() {
                            Some(flat)
                        } else if nested.is_file() {
                            Some(nested)
                        } else {
                            None
                        }
                    })
                    .ok_or_else(|| {
                        mlua::Error::RuntimeError(format!("module '{name}' not found"))
                    })?;
                let src = std::fs::read_to_string(&path).map_err(|e| {
                    mlua::Error::RuntimeError(format!("{}: {e}", path.display()))
                })?;
                // Guard against a module that requires itself (directly or via
                // a cycle) recursing forever: seed the cache before running.
                loaded.set(name.as_str(), true)?;
                let result = lua
                    .load(&src)
                    .set_name(path.display().to_string())
                    .call::<_, Value>(name.as_str());
                match result {
                    Ok(v) => v,
                    Err(e) => {
                        // The guard above must not become a permanent false
                        // "successfully loaded" cache entry on failure — real
                        // `require()` doesn't cache a failed load either, so
                        // the next attempt (after e.g. a missing primitive
                        // gets implemented) retries from scratch instead of
                        // forever returning a bare `true`.
                        loaded.set(name.as_str(), Value::Nil)?;
                        return Err(e);
                    }
                }
            };
            let value = if matches!(value, Value::Nil) { Value::Boolean(true) } else { value };
            loaded.set(name.as_str(), value.clone())?;
            Ok(value)
        })?;
        lua.globals().set("require", require)?;
        Ok(())
    }

    /// Install `vim.keymap.set(mode, lhs, rhs, opts)`, matching Neovim's
    /// signature: `rhs` is a string of keys to replay *or* a Lua callback, and
    /// `opts.desc` describes the mapping.
    ///
    /// A string right-hand side becomes a real mapping in the session's table,
    /// so a plugin's `vim.keymap.set('n', '<leader>x', ':Foo<CR>')` behaves
    /// exactly like the same line in config. A callback is stored as a
    /// `LuaRef` and is still trigger-only — dispatching one needs the typeahead
    /// layer to learn how to call back into Lua mid-keystroke.
    ///
    /// `desc` is kept in both cases. It is the only source of the text the
    /// keybinding help shows, so dropping it here would mean a plugin's
    /// mappings could never be described.
    fn install_keymap(&self, vim: &Table) -> mlua::Result<()> {
        let keymap = self.lua.create_table()?;
        let ctx = self.ctx.clone();
        let store = self.store.clone();
        let set = self.lua.create_function(
            move |lua, (mode, lhs, rhs, opts): (String, String, Value, Option<Table>)| {
                let desc = match &opts {
                    Some(o) => o.get::<_, Option<String>>("desc")?,
                    None => None,
                };
                match rhs {
                    Value::Function(cb) => {
                        let id = store.store(lua, cb)?;
                        ctx.borrow_mut().keymaps.insert((mode, lhs), (id, desc));
                    }
                    Value::String(s) => {
                        let rhs = s.to_str()?.to_string();
                        let mode = ctrlvim_editor::keymap::MapMode::parse(&mode);
                        ctx.borrow_mut()
                            .session
                            .keymap
                            .set_with_desc(mode, &lhs, &rhs, desc)
                            .map_err(mlua::Error::runtime)?;
                    }
                    other => {
                        return Err(mlua::Error::runtime(format!(
                            "vim.keymap.set: rhs must be a string or a function, got {}",
                            other.type_name()
                        )))
                    }
                }
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

        // `nvim_exec_autocmds(event, opts)` — fire matching autocmds *now*,
        // invoking their Lua callbacks directly. This can't be a plain
        // `#[ctrlvim_api]` function like the rest of this table: those only
        // see `ApiContext`, which stores a callback as an opaque `LuaRef`
        // but has no `Lua`/registry to actually call it — that's `Host`'s
        // job, the same split `fire_autocmd` already has.
        let ctx_exec = self.ctx.clone();
        let store_exec = self.store.clone();
        let exec_autocmds = self.lua.create_function(move |lua, (event, opts): (String, Option<Table>)| {
            let pattern = match &opts {
                Some(o) => o.get::<_, Option<String>>("pattern")?,
                None => None,
            };
            let buf = match &opts {
                Some(o) => o.get::<_, Option<i64>>("buf")?,
                None => None,
            };
            let data = match &opts {
                Some(o) => o.get::<_, Value>("data")?,
                None => Value::Nil,
            };
            let file = match pattern {
                Some(p) => p,
                None => ctx_exec.borrow().editor().cur_buffer().name.clone().unwrap_or_default(),
            };
            let callbacks = ctx_exec.borrow_mut().autocmds.fire(&event, &file);
            for cb in callbacks {
                if let CallbackRef::Lua(LuaRef(id)) = cb {
                    if let Some(func) = store_exec.get(lua, id)? {
                        let arg = lua.create_table()?;
                        arg.set("event", event.clone())?;
                        arg.set("file", file.clone())?;
                        arg.set("match", file.clone())?;
                        if let Some(b) = buf {
                            arg.set("buf", b)?;
                        }
                        arg.set("data", data.clone())?;
                        let _: () = func.call(arg)?;
                    }
                }
            }
            Ok(())
        })?;
        api.set("nvim_exec_autocmds", exec_autocmds)?;

        // `nvim_get_runtime_file(pattern, all)` — search every registered
        // runtime path (the same list `require()` searches, see
        // `install_require`) for files matching `pattern`, which may end in
        // a `*.ext` glob. This is *not* the `require()` module system: it's
        // Neovim's separate `'runtimepath'`-file convention (`ftplugin/`,
        // `syntax/`, and — what `vim.lsp.config` actually uses to find a
        // plugin's `lsp/<name>.lua` server presets — `lsp/`). `all = false`
        // stops at the first match (earlier-registered paths win); `true`
        // collects every match, which is how `vim.lsp.config` lets a
        // later-registered path's `lsp/foo.lua` extend an earlier one's
        // rather than replace it.
        let runtime_paths_rtf = self.runtime_paths.clone();
        let get_runtime_file = self.lua.create_function(move |_, (pattern, all): (String, bool)| {
            let mut matches: Vec<PathBuf> = Vec::new();
            for root in runtime_paths_rtf.borrow().iter() {
                let candidate = root.join(&pattern);
                if let Some(star_at) = pattern.find('*') {
                    let dir = candidate.parent().map(Path::to_path_buf).unwrap_or_default();
                    let fname_pattern = &pattern[pattern.rfind('/').map(|i| i + 1).unwrap_or(0)..];
                    let _ = star_at;
                    let (prefix, suffix) = fname_pattern.split_once('*').unwrap_or((fname_pattern, ""));
                    if let Ok(entries) = std::fs::read_dir(&dir) {
                        let mut hits: Vec<PathBuf> = entries
                            .flatten()
                            .filter_map(|e| {
                                let name = e.file_name().to_str()?.to_string();
                                (name.starts_with(prefix) && name.ends_with(suffix)).then(|| e.path())
                            })
                            .collect();
                        hits.sort();
                        matches.extend(hits);
                    }
                } else if candidate.is_file() {
                    matches.push(candidate);
                }
                if !all && !matches.is_empty() {
                    break;
                }
            }
            Ok(matches.into_iter().map(|p| p.to_string_lossy().into_owned()).collect::<Vec<_>>())
        })?;
        api.set("nvim_get_runtime_file", get_runtime_file)?;

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

        // `uv.new_pipe(ipc)` — an unbound handle until it's named in a
        // `uv.spawn` `stdio` table, matching real luv usage (`vim.lsp.rpc`
        // always creates its three pipes immediately before spawning with
        // them). `read_start`/`write`/`close` are no-ops on a pipe that never
        // got bound, rather than erroring — the same forgiving posture as the
        // rest of this binding layer.
        let next_pipe_id = self.next_pipe_id.clone();
        let pipe_roles = self.pipe_roles.clone();
        let pipe_read_cbs = self.pipe_read_cbs.clone();
        let job_stdin = self.job_stdin.clone();
        let new_pipe = self.lua.create_function(move |lua, _ipc: Option<bool>| {
            let id = {
                let mut n = next_pipe_id.borrow_mut();
                let v = *n;
                *n += 1;
                v
            };
            let handle = lua.create_table()?;
            handle.set("__pipe_id", id)?;

            let cbs = pipe_read_cbs.clone();
            let read_start = lua.create_function(move |lua, (_self, cb): (Table, Function)| {
                let key = lua.create_registry_value(cb)?;
                cbs.borrow_mut().insert(id, Rc::new(key));
                Ok(())
            })?;
            handle.set("read_start", read_start)?;

            let cbs2 = pipe_read_cbs.clone();
            let read_stop = lua.create_function(move |_, _self: Table| {
                cbs2.borrow_mut().remove(&id);
                Ok(())
            })?;
            handle.set("read_stop", read_stop)?;

            let roles = pipe_roles.clone();
            let stdins = job_stdin.clone();
            let write = lua.create_function(move |_, (_self, data): (Table, mlua::String)| {
                if let Some(PipeRole::Stdin { job_id }) = roles.borrow().get(&id).copied() {
                    if let Some(stdin) = stdins.borrow().get(&job_id) {
                        stdin.write(data.as_bytes().to_vec());
                    }
                }
                Ok(())
            })?;
            handle.set("write", write)?;

            let roles2 = pipe_roles.clone();
            let cbs3 = pipe_read_cbs.clone();
            let stdins2 = job_stdin.clone();
            let close = lua.create_function(move |_, _self: Table| {
                cbs3.borrow_mut().remove(&id);
                // Closing the *stdin* pipe must actually drop its `JobStdin`
                // (not just forget the binding) — that's what closes the
                // real OS pipe and lets a process reading until EOF (`cat`,
                // most LSP servers on `exit`) proceed. Closing a stdout/
                // stderr pipe just stops delivering reads; the process keeps
                // running until it exits on its own.
                if let Some(PipeRole::Stdin { job_id }) = roles2.borrow_mut().remove(&id) {
                    stdins2.borrow_mut().remove(&job_id);
                }
                Ok(())
            })?;
            handle.set("close", close)?;
            handle.set("is_closing", lua.create_function(|_, _self: Table| Ok(false))?)?;

            Ok(handle)
        })?;
        uv.set("new_pipe", new_pipe)?;

        // `uv.spawn(path, opts, on_exit)` — `opts.args`/`opts.cwd` map
        // straight onto `Jobs::spawn_persistent`; `opts.stdio[1..3]`, if
        // present, bind those `new_pipe()` handles to this job's
        // stdin/stdout/stderr so later `pipe:read_start`/`pipe:write` calls
        // reach the right stream. `opts.env`/`opts.detached` are accepted and
        // silently ignored — harmless for callers that set them defensively,
        // but not yet honored.
        let jobs = self.jobs.clone();
        let pipe_roles_s = self.pipe_roles.clone();
        let job_stdin_s = self.job_stdin.clone();
        let job_on_exit_s = self.job_on_exit.clone();
        let spawn = self.lua.create_function(
            move |lua, (path, opts, on_exit): (String, Table, Option<Function>)| {
                let args: Vec<String> = match opts.get::<_, Option<Table>>("args")? {
                    Some(t) => t.sequence_values::<String>().collect::<mlua::Result<Vec<_>>>()?,
                    None => Vec::new(),
                };
                let cwd = opts
                    .get::<_, Option<String>>("cwd")?
                    .map(PathBuf::from)
                    .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());

                let (job_id, stdin) = jobs.borrow_mut().spawn_persistent(&path, &args, &cwd);
                job_stdin_s.borrow_mut().insert(job_id, stdin);

                if let Some(stdio) = opts.get::<_, Option<Table>>("stdio")? {
                    if let Ok(pipe) = stdio.get::<_, Table>(1) {
                        if let Ok(pid) = pipe.get::<_, u64>("__pipe_id") {
                            pipe_roles_s.borrow_mut().insert(pid, PipeRole::Stdin { job_id });
                        }
                    }
                    if let Ok(pipe) = stdio.get::<_, Table>(2) {
                        if let Ok(pid) = pipe.get::<_, u64>("__pipe_id") {
                            pipe_roles_s.borrow_mut().insert(pid, PipeRole::Stdout { job_id });
                        }
                    }
                    if let Ok(pipe) = stdio.get::<_, Table>(3) {
                        if let Ok(pid) = pipe.get::<_, u64>("__pipe_id") {
                            pipe_roles_s.borrow_mut().insert(pid, PipeRole::Stderr { job_id });
                        }
                    }
                }

                if let Some(cb) = on_exit {
                    let key = lua.create_registry_value(cb)?;
                    job_on_exit_s.borrow_mut().insert(job_id, Rc::new(key));
                }

                let handle = lua.create_table()?;
                handle.set("__job_id", job_id)?;
                let kill_stdin = job_stdin_s.clone();
                let kill = lua.create_function(move |_, (_self, _signal): (Table, Option<Value>)| {
                    // No real signal delivery: dropping stdin is enough to end
                    // a well-behaved server/CLI reading until EOF, which is
                    // the shutdown path `vim.lsp`'s `client.stop()` actually
                    // exercises. A process that ignores EOF and needs a hard
                    // kill is a known gap — nothing here claims to send
                    // SIGKILL, so this doesn't silently misbehave.
                    kill_stdin.borrow_mut().remove(&job_id);
                    Ok(())
                })?;
                handle.set("kill", kill.clone())?;
                handle.set("close", kill)?;
                handle.set("is_closing", lua.create_function(|_, _self: Table| Ok(false))?)?;

                Ok((handle, job_id))
            },
        )?;
        uv.set("spawn", spawn)?;

        // `uv.cwd()`.
        let cwd_fn = self.lua.create_function(|_, ()| {
            Ok(std::env::current_dir().ok().and_then(|p| p.to_str().map(str::to_string)))
        })?;
        uv.set("cwd", cwd_fn)?;

        // `uv.now()` / `uv.hrtime()` — milliseconds / nanoseconds since an
        // arbitrary epoch. Real luv caches `now()` per loop iteration; we
        // just read the clock, which is observably fine for the timeout/
        // logging math callers do with these.
        let now_fn = self.lua.create_function(|_, ()| {
            Ok(std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0))
        })?;
        uv.set("now", now_fn)?;
        let hrtime_fn = self.lua.create_function(|_, ()| {
            Ok(std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as i64)
                .unwrap_or(0))
        })?;
        uv.set("hrtime", hrtime_fn)?;

        // `uv.fs_stat(path)` — the synchronous, no-callback form (what
        // `lspconfig`'s root-dir detection uses to check `.git`/`Cargo.toml`-
        // style markers). Returns `nil, err` for a path that doesn't exist,
        // matching luv rather than raising.
        let fs_stat = self.lua.create_function(|lua, path: String| {
            match std::fs::metadata(&path) {
                Ok(meta) => {
                    let t = lua.create_table()?;
                    let kind = if meta.is_dir() {
                        "directory"
                    } else if meta.is_file() {
                        "file"
                    } else {
                        "other"
                    };
                    t.set("type", kind)?;
                    t.set("size", meta.len())?;
                    Ok((Value::Table(t), Value::Nil))
                }
                Err(e) => Ok((Value::Nil, Value::String(lua.create_string(&e.to_string())?))),
            }
        })?;
        uv.set("fs_stat", fs_stat)?;

        // `uv.os_uname()` — real luv shape is `{sysname, release, version,
        // machine}`; `vim.lsp.protocol` only reads `sysname` (to special-case
        // Windows path handling), so that's the one field given real values.
        let os_uname = self.lua.create_function(|lua, ()| {
            let t = lua.create_table()?;
            let sysname = match std::env::consts::OS {
                "macos" => "Darwin",
                "windows" => "Windows_NT",
                "linux" => "Linux",
                other => other,
            };
            t.set("sysname", sysname)?;
            t.set("release", "")?;
            t.set("version", "")?;
            t.set("machine", std::env::consts::ARCH)?;
            Ok(t)
        })?;
        uv.set("os_uname", os_uname)?;

        let os_getpid = self.lua.create_function(|_, ()| Ok(std::process::id() as i64))?;
        uv.set("os_getpid", os_getpid)?;

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

    /// Fire `buf`'s `nvim_buf_attach` `on_lines` callback if its content
    /// changed since the last call (see [`ApiContext::check_buf_watcher`] for
    /// how the changed range is derived).
    ///
    /// **What calls this today:** nothing automatically for interactive
    /// typing — `ctrlvim-tui`'s `Session`/`Editor` (what the user actually
    /// types into) is a separate instance from this `Host`'s own `Editor`
    /// (see `ctrlvim-core::Ctrlvim`'s doc comment on that pre-existing gap),
    /// so per-keystroke edits don't reach here yet. Any edit made *through
    /// this Lua API* (`nvim_buf_set_lines` and friends, which is how a
    /// plugin itself edits) already goes through the same `ApiContext`, so
    /// calling this right after running Lua does cover plugin-driven edits —
    /// wiring real interactive typing through is a follow-up integration,
    /// not something this method silently gets wrong.
    pub fn notify_buf_lines_changed(&self, buf: ctrlvim_types::BufferId) -> mlua::Result<()> {
        let hit = self.ctx.borrow_mut().check_buf_watcher(buf);
        let Some((LuaRef(id), tick, firstline, lastline, new_lastline)) = hit else {
            return Ok(());
        };
        if let Some(func) = self.store.get(&self.lua, id)? {
            // Real Neovim's `on_lines` signature: `(the literal string
            // "lines", buf, changedtick, firstline, lastline, new_lastline,
            // byte_count, ...)`. `byte_count` is approximated as 0 — this
            // engine doesn't track it — which is only a gap for a caller
            // that uses it for something other than a rough size hint (the
            // vendored `vim.lsp` change-tracking, once it lands, re-derives
            // the actual edit text from the buffer itself).
            let _: () = func.call(("lines", buf.0 as i64, tick as i64, firstline as i64, lastline as i64, new_lastline as i64, 0i64))?;
        }
        Ok(())
    }

    /// Drive the event loop for up to `timeout`, invoking Lua callbacks for any
    /// timers that fire. Returns the number of callbacks invoked. This is the
    /// model of `state_enter`'s wait-then-process-`K_EVENT` cycle: waiting
    /// happens on tokio, callback invocation happens here on the editor thread.
    pub fn run_events(&self, timeout: Duration) -> mlua::Result<usize> {
        // Block for the first event, then drain whatever else is ready.
        let mut batch = Vec::new();
        if let Some(ev) = self.events.wait(timeout) {
            batch.push(ev);
            batch.extend(self.events.drain());
        }
        self.process_batch(batch)
    }

    /// Non-blocking version of [`Self::run_events`], plus draining
    /// `vim.schedule`'s queue: the embedder's per-tick "pick up whatever the
    /// Lua host has ready" call, the equivalent of `App::poll_jobs` but for
    /// this host's own timers/process I/O *and* deferred callbacks.
    ///
    /// `vim.lsp.rpc` (and plenty of ordinary plugin code) defers its actual
    /// message handling via `vim.schedule` — matching real Neovim's
    /// fast-event-safety model, where a libuv callback schedules work rather
    /// than running it inline. Skipping the `run_scheduled` half here would
    /// mean that work never runs at all: an LSP response would arrive (and
    /// even get logged), but the callback that acts on it would sit queued
    /// forever.
    pub fn poll(&self) -> mlua::Result<usize> {
        let batch = self.events.drain();
        let from_events = self.process_batch(batch)?;
        let from_scheduled = self.run_scheduled()?;
        Ok(from_events + from_scheduled)
    }

    fn process_batch(&self, batch: Vec<Event>) -> mlua::Result<usize> {
        let mut invoked = 0;
        for ev in batch {
            match ev {
                Event::TimerFired(tid) => {
                    let luaref = self.timer_cbs.borrow().get(&tid).copied();
                    if let Some(id) = luaref {
                        if let Some(func) = self.store.get(&self.lua, id)? {
                            let _: () = func.call(())?;
                            invoked += 1;
                        }
                    }
                }
                Event::ProcessStdout { id, data } => {
                    invoked += self.dispatch_pipe_read(id, false, Some(data))?;
                }
                Event::ProcessStderr { id, data } => {
                    invoked += self.dispatch_pipe_read(id, true, Some(data))?;
                }
                Event::ProcessExit { id, code } => {
                    // A `read_start` callback's contract is `(err, chunk)`
                    // with `chunk == nil` on EOF — a process exiting is
                    // exactly that, on whichever of its pipes had a callback.
                    invoked += self.dispatch_pipe_read(id, false, None)?;
                    invoked += self.dispatch_pipe_read(id, true, None)?;

                    if let Some(key) = self.job_on_exit.borrow_mut().remove(&id) {
                        if let Ok(func) = self.lua.registry_value::<Function>(&key) {
                            let _: () = func.call((code, 0i64))?;
                            invoked += 1;
                        }
                        if let Ok(key) = Rc::try_unwrap(key) {
                            self.lua.remove_registry_value(key)?;
                        }
                    }
                    self.job_stdin.borrow_mut().remove(&id);
                    let dead_pipes: Vec<u64> = self
                        .pipe_roles
                        .borrow()
                        .iter()
                        .filter(|(_, role)| role.job_id() == id)
                        .map(|(pid, _)| *pid)
                        .collect();
                    for pid in dead_pipes {
                        self.pipe_roles.borrow_mut().remove(&pid);
                        self.pipe_read_cbs.borrow_mut().remove(&pid);
                    }
                }
                _ => {}
            }
        }
        Ok(invoked)
    }

    /// Find the pipe bound as job `id`'s stdout (or stderr, if `stderr`) and,
    /// if it has a `read_start` callback, invoke it with `(nil, chunk)` —
    /// `chunk` is `None` (Lua `nil`) to signal EOF, matching luv. Returns 1 if
    /// a callback ran, 0 otherwise, so callers can fold it into `run_events`'s
    /// invocation count.
    fn dispatch_pipe_read(&self, id: u64, stderr: bool, chunk: Option<Vec<u8>>) -> mlua::Result<usize> {
        let pipe_id = self.pipe_roles.borrow().iter().find_map(|(pid, role)| {
            let matches = match role {
                PipeRole::Stdout { job_id } => !stderr && *job_id == id,
                PipeRole::Stderr { job_id } => stderr && *job_id == id,
                PipeRole::Stdin { .. } => false,
            };
            matches.then_some(*pid)
        });
        let Some(pipe_id) = pipe_id else { return Ok(0) };
        let key = self.pipe_read_cbs.borrow().get(&pipe_id).cloned();
        let Some(key) = key else { return Ok(0) };
        let func: Function = self.lua.registry_value(&key)?;
        let arg = match chunk {
            Some(bytes) => Value::String(self.lua.create_string(&bytes)?),
            None => Value::Nil,
        };
        let _: () = func.call((Value::Nil, arg))?;
        Ok(1)
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

    /// `vim.opt` / `vim.o` — option access from Lua.
    ///
    /// Both are the same metatable-backed proxy: reads go through
    /// `ctrlvim_get_option_value`, writes go through the engine's `:set`, so
    /// Lua and the command line share one notion of what an option means.
    fn install_opt(&self, vim: &Table) -> mlua::Result<()> {
        let proxy = self.lua.create_table()?;
        let meta = self.lua.create_table()?;

        let ctx = self.ctx.clone();
        let store = self.store.clone();
        let index = self
            .lua
            .create_function(move |lua, (_t, key): (Table, String)| {
                let mut c = ctx.borrow_mut();
                let value = ctrlvim_api::registry::call(
                    &mut c,
                    "ctrlvim_get_option_value",
                    &[ctrlvim_types::Object::str(key)],
                )
                .map_err(|e| mlua::Error::RuntimeError(e.to_string()))?;
                crate::convert::to_lua(lua, &value, &store)
            })?;
        meta.set("__index", index)?;

        let ctx = self.ctx.clone();
        let newindex = self
            .lua
            .create_function(move |_, (_t, key, value): (Table, String, Value)| {
                // Render the assignment as a `:set` argument and let the engine
                // parse it — same path, same validation, same errors.
                let arg = match value {
                    Value::Boolean(true) => key,
                    Value::Boolean(false) => format!("no{key}"),
                    Value::Integer(n) => format!("{key}={n}"),
                    Value::Number(n) => format!("{key}={n}"),
                    Value::String(s) => format!("{key}={}", s.to_str()?),
                    _ => {
                        return Err(mlua::Error::RuntimeError(format!(
                            "vim.opt.{key}: unsupported value type"
                        )))
                    }
                };
                ctx.borrow_mut().exec_ex(&format!("set {arg}"));
                Ok(())
            })?;
        meta.set("__newindex", newindex)?;
        proxy.set_metatable(Some(meta));

        vim.set("opt", proxy.clone())?;
        vim.set("o", proxy)?;
        Ok(())
    }

    /// `vim.g` — global variables, shared with Vimscript's `g:` scope so
    /// `vim.g.x` and `:let g:x` are the same variable.
    fn install_vars(&self, vim: &Table) -> mlua::Result<()> {
        let proxy = self.lua.create_table()?;
        let meta = self.lua.create_table()?;

        let ctx = self.ctx.clone();
        let store = self.store.clone();
        let index = self
            .lua
            .create_function(move |lua, (_t, key): (Table, String)| {
                let c = ctx.borrow();
                match c.get_global(&key) {
                    Some(v) => crate::convert::to_lua(lua, &v, &store),
                    // An unset global reads as nil, as it does in Neovim.
                    None => Ok(Value::Nil),
                }
            })?;
        meta.set("__index", index)?;

        let ctx = self.ctx.clone();
        let newindex = self
            .lua
            .create_function(move |_, (_t, key, value): (Table, String, Value)| {
                let obj = match value {
                    Value::Boolean(b) => ctrlvim_types::Object::Boolean(b),
                    Value::Integer(n) => ctrlvim_types::Object::Integer(n),
                    Value::Number(n) => ctrlvim_types::Object::Integer(n as i64),
                    Value::String(s) => ctrlvim_types::Object::str(s.to_str()?.to_string()),
                    Value::Nil => ctrlvim_types::Object::Nil,
                    _ => {
                        return Err(mlua::Error::RuntimeError(format!(
                            "vim.g.{key}: unsupported value type"
                        )))
                    }
                };
                ctx.borrow_mut().set_global(&key, obj);
                Ok(())
            })?;
        meta.set("__newindex", newindex)?;
        proxy.set_metatable(Some(meta));
        vim.set("g", proxy)?;
        Ok(())
    }

    /// `vim.cmd` — run an Ex command / Vimscript line from Lua.
    fn install_cmd(&self, vim: &Table) -> mlua::Result<()> {
        let ctx = self.ctx.clone();
        let cmd = self.lua.create_function(move |_, src: String| {
            let line = src.trim().trim_start_matches(':').to_string();
            let mut c = ctx.borrow_mut();
            // Ex commands go through the session; anything the Ex layer doesn't
            // recognize is Vimscript (`let`, `call`, `echo`, …).
            if ctrlvim_editor::is_ex_command(&line) {
                c.exec_ex(&line);
            } else {
                c.exec_vimscript(&line)
                    .map_err(|e| mlua::Error::RuntimeError(e.to_string()))?;
            }
            Ok(())
        })?;
        vim.set("cmd", cmd)?;
        Ok(())
    }

    /// The small `vim.*` stdlib helpers plugins reach for constantly.
    fn install_stdlib(&self, vim: &Table) -> mlua::Result<()> {
        let lua = &self.lua;

        // `vim.notify(msg, level)` — collected for the frontend to display.
        let notices = self.notices.clone();
        let notify = lua.create_function(move |_, (msg, level): (String, Option<i64>)| {
            notices.borrow_mut().push((level.unwrap_or(2), msg));
            Ok(())
        })?;
        vim.set("notify", notify)?;

        // `vim.log.levels` — the standard severity enum `vim.notify`,
        // `vim.diagnostic`, and countless plugins key off of.
        let log = lua.create_table()?;
        let levels = lua.create_table()?;
        levels.set("TRACE", 0)?;
        levels.set("DEBUG", 1)?;
        levels.set("INFO", 2)?;
        levels.set("WARN", 3)?;
        levels.set("ERROR", 4)?;
        levels.set("OFF", 5)?;
        log.set("levels", levels)?;
        vim.set("log", log)?;

        // `vim.v` — Vim's special `v:` variables. A real, but partial and
        // read-only, subset: this engine doesn't route these through the
        // Vimscript interpreter's actual `v:` scope (unlike `vim.g`/`vim.opt`,
        // which do share state with their Vimscript counterparts) — these are
        // static/sensible defaults, not live editor state. `v:count`/
        // `v:char`/etc. genuinely changing per-keystroke is a real gap.
        let v = lua.create_table()?;
        v.set("maxcol", 2147483647i64)?;
        v.set("count", 0)?;
        v.set("count1", 1)?;
        v.set("char", "")?;
        v.set("event", lua.create_table()?)?;
        v.set("lnum", 0)?;
        v.set("errmsg", "")?;
        v.set("warningmsg", "")?;
        v.set("shell_error", 0)?;
        vim.set("v", v)?;

        // `vim.version()` — a version table with a `__tostring` metamethod
        // (`vim.lsp.client` sends `tostring(vim.version())` as the LSP
        // `clientInfo.version`). Reports ctrlvim's own version, not a
        // pretend Neovim version — a server that branches on this exact
        // string for a Neovim-specific quirk is a known, narrow gap.
        let version_fn = lua.create_function(|lua, ()| {
            let t = lua.create_table()?;
            t.set("major", 0)?;
            t.set("minor", 1)?;
            t.set("patch", 0)?;
            let meta = lua.create_table()?;
            let tostring = lua.create_function(|_, t: Table| {
                let (major, minor, patch): (i64, i64, i64) = (t.get("major")?, t.get("minor")?, t.get("patch")?);
                Ok(format!("{major}.{minor}.{patch}"))
            })?;
            meta.set("__tostring", tostring)?;
            t.set_metatable(Some(meta));
            Ok(t)
        })?;
        vim.set("version", version_fn)?;

        // `vim._empty_dict_mt` / `vim.empty_dict()` — the metatable marker
        // real Neovim uses to say "encode this table as a JSON *object*
        // (`{}`), even though it's empty" — Lua can't otherwise tell an
        // empty array from an empty object. `vim.lsp.client` sends exactly
        // one of these as the `initialized` notification's params.
        let empty_dict_mt = lua.create_table()?;
        vim.set("_empty_dict_mt", empty_dict_mt.clone())?;
        // `Table<'lua>` can't be captured by a `'static` closure (same
        // reason as `install_require`'s `loaded`/`preload` tables) — stash it
        // in the registry and re-fetch through each closure's own `lua`
        // parameter instead.
        let empty_dict_mt_key = Rc::new(lua.create_registry_value(empty_dict_mt.clone())?);
        let empty_dict_mt_key_for_fn = empty_dict_mt_key.clone();
        let empty_dict = lua.create_function(move |lua, ()| {
            let mt: Table = lua.registry_value(&empty_dict_mt_key_for_fn)?;
            let t = lua.create_table()?;
            t.set_metatable(Some(mt));
            Ok(t)
        })?;
        vim.set("empty_dict", empty_dict)?;

        // `vim.json.encode`/`decode` — real JSON, real serde_json, since
        // `vim.lsp.rpc` frames every request/response/notification as JSON.
        // `encode` is a hand-rolled Lua-value walk rather than mlua's generic
        // serde bridge specifically so `vim._empty_dict_mt`-tagged tables (at
        // any nesting depth) encode as `{}` rather than being guessed at.
        // Simplification: JSON `null` decodes to Lua `nil` rather than the
        // real `vim.NIL` sentinel, so a `null` *value* inside a table is
        // indistinguishable from an absent key — correct for the common
        // "check if a field is present/truthy" case, a known gap for code
        // that specifically branches on "explicitly null vs. absent".
        let json = lua.create_table()?;
        let empty_dict_mt_key_for_encode = empty_dict_mt_key.clone();
        let encode = lua.create_function(move |lua, value: Value| {
            let mt: Table = lua.registry_value(&empty_dict_mt_key_for_encode)?;
            let json_value = lua_value_to_json(&value, &mt)?;
            serde_json::to_string(&json_value).map_err(|e| mlua::Error::RuntimeError(e.to_string()))
        })?;
        json.set("encode", encode)?;
        let decode = lua.create_function(|lua, text: String| {
            let json_value: serde_json::Value =
                serde_json::from_str(&text).map_err(|e| mlua::Error::RuntimeError(e.to_string()))?;
            lua.to_value(&json_value)
        })?;
        json.set("decode", decode)?;
        vim.set("json", json)?;

        // `vim._with_c(context, callback)` — the C primitive behind
        // `vim._with({buf=..., win=...}, fn)`, which several vendored
        // runtime files (`vim.hl.range`, `vim.fn`-adjacent helpers) use to
        // run a callback as if a different buffer/window were current, then
        // restore. Returns a pcall-shaped array (`{ok, ...results}` or
        // `{false, err}`) — the Lua-side `vim._with` wrapper (in
        // `vim._core.shared`, vendored) unpacks it.
        let ctx_with = self.ctx.clone();
        let with_c = self.lua.create_function(move |lua, (context, callback): (Table, Function)| {
            let want_buf = context.get::<_, Option<i64>>("buf")?;
            let want_win = context.get::<_, Option<i64>>("win")?;

            let restore_win = ctx_with.borrow().editor().current_window_id();
            let restore_buf = ctx_with.borrow().editor().window(restore_win).map(|w| w.buffer);
            {
                let mut c = ctx_with.borrow_mut();
                let ed = c.editor_mut();
                if let Some(w) = want_win {
                    ed.focus_window(ctrlvim_types::WindowId(w as u32));
                }
                if let Some(b) = want_buf {
                    let cur = ed.current_window_id();
                    if let Some(win) = ed.window_mut(cur) {
                        win.buffer = ctrlvim_types::BufferId(b as u32);
                    }
                }
            }

            let result: mlua::Result<MultiValue> = callback.call(());

            {
                let mut c = ctx_with.borrow_mut();
                let ed = c.editor_mut();
                ed.focus_window(restore_win);
                if let Some(buf) = restore_buf {
                    if let Some(win) = ed.window_mut(restore_win) {
                        win.buffer = buf;
                    }
                }
            }

            let out = lua.create_table()?;
            match result {
                Ok(vals) => {
                    out.set(1, true)?;
                    for (i, v) in vals.into_iter().enumerate() {
                        out.set(i + 2, v)?;
                    }
                }
                Err(e) => {
                    out.set(1, false)?;
                    out.set(2, e.to_string())?;
                }
            }
            Ok(out)
        })?;
        vim.set("_with_c", with_c)?;

        // `vim.schedule(fn)` — defer until the editor is back at a safe point.
        // Callbacks queue here and the host drains them via `run_scheduled`.
        let store = self.store.clone();
        let scheduled = self.scheduled.clone();
        let schedule = lua.create_function(move |lua, cb: Function| {
            let id = store.store(lua, cb)?;
            scheduled.borrow_mut().push(id);
            Ok(())
        })?;
        vim.set("schedule", schedule)?;

        // `vim.split(s, sep)`
        let split = lua.create_function(|lua, (s, sep): (String, Option<String>)| {
            let sep = sep.unwrap_or_else(|| " ".to_string());
            let out = lua.create_table()?;
            if sep.is_empty() {
                out.set(1, s)?;
                return Ok(out);
            }
            for (i, part) in s.split(sep.as_str()).enumerate() {
                out.set(i + 1, part)?;
            }
            Ok(out)
        })?;
        vim.set("split", split)?;

        // `vim.trim(s)`
        let trim = lua.create_function(|_, s: String| Ok(s.trim().to_string()))?;
        vim.set("trim", trim)?;

        // `vim.startswith` / `vim.endswith`
        let startswith =
            lua.create_function(|_, (s, prefix): (String, String)| Ok(s.starts_with(&prefix)))?;
        vim.set("startswith", startswith)?;
        let endswith =
            lua.create_function(|_, (s, suffix): (String, String)| Ok(s.ends_with(&suffix)))?;
        vim.set("endswith", endswith)?;

        // `vim.tbl_count`
        let tbl_count = lua.create_function(|_, t: Table| {
            let mut n = 0;
            for pair in t.pairs::<Value, Value>() {
                pair?;
                n += 1;
            }
            Ok(n)
        })?;
        vim.set("tbl_count", tbl_count)?;

        // `vim.tbl_isempty`
        let tbl_isempty =
            lua.create_function(|_, t: Table| Ok(t.pairs::<Value, Value>().next().is_none()))?;
        vim.set("tbl_isempty", tbl_isempty)?;

        // `vim.tbl_keys` / `vim.tbl_values`
        let tbl_keys = lua.create_function(|lua, t: Table| {
            let out = lua.create_table()?;
            for (i, pair) in t.pairs::<Value, Value>().enumerate() {
                out.set(i + 1, pair?.0)?;
            }
            Ok(out)
        })?;
        vim.set("tbl_keys", tbl_keys)?;
        let tbl_values = lua.create_function(|lua, t: Table| {
            let out = lua.create_table()?;
            for (i, pair) in t.pairs::<Value, Value>().enumerate() {
                out.set(i + 1, pair?.1)?;
            }
            Ok(out)
        })?;
        vim.set("tbl_values", tbl_values)?;

        // `vim.tbl_extend(behavior, a, b)` — "force" or "keep" on conflict.
        let tbl_extend =
            lua.create_function(|lua, (behavior, a, b): (String, Table, Table)| {
                let out = lua.create_table()?;
                for pair in a.pairs::<Value, Value>() {
                    let (k, v) = pair?;
                    out.set(k, v)?;
                }
                let force = behavior == "force";
                for pair in b.pairs::<Value, Value>() {
                    let (k, v) = pair?;
                    if force || out.get::<Value, Value>(k.clone())? == Value::Nil {
                        out.set(k, v)?;
                    }
                }
                Ok(out)
            })?;
        vim.set("tbl_extend", tbl_extend)?;

        Ok(())
    }

    /// `vim.fs.*` — the path helpers `vim.lsp`'s logging and root-dir
    /// detection reach for. A small, real subset (not the full module):
    /// `joinpath`, `dirname`, `basename`, and `root` (walk up from a start
    /// path looking for any of a list of marker names — what modern
    /// lspconfig-style server configs use instead of their own
    /// `root_pattern` helper).
    fn install_fs(&self, vim: &Table) -> mlua::Result<()> {
        let fs = self.lua.create_table()?;

        let joinpath = self.lua.create_function(|_, parts: MultiValue| {
            let strs: Vec<String> = parts
                .into_iter()
                .map(|v| match v {
                    Value::String(s) => Ok(s.to_str()?.to_string()),
                    other => Err(mlua::Error::RuntimeError(format!(
                        "vim.fs.joinpath: expected string, got {}",
                        other.type_name()
                    ))),
                })
                .collect::<mlua::Result<_>>()?;
            let mut path = std::path::PathBuf::new();
            for s in strs {
                path.push(s);
            }
            Ok(path.to_string_lossy().into_owned())
        })?;
        fs.set("joinpath", joinpath)?;

        let dirname = self.lua.create_function(|_, path: String| {
            Ok(std::path::Path::new(&path).parent().map(|p| p.to_string_lossy().into_owned()))
        })?;
        fs.set("dirname", dirname)?;

        let basename = self.lua.create_function(|_, path: String| {
            Ok(std::path::Path::new(&path).file_name().map(|n| n.to_string_lossy().into_owned()))
        })?;
        fs.set("basename", basename)?;

        // `vim.fs.root(start, markers)` — `start` a buffer number or path;
        // `markers` a string or array of strings, each a file/dir name that
        // marks a project root. Walks up from `start` and returns the first
        // ancestor directory containing any marker, or `nil`.
        let ctx_root = self.ctx.clone();
        let root = self.lua.create_function(move |_, (start, markers): (Value, Value)| {
            let start_path = match start {
                Value::String(s) => s.to_str()?.to_string(),
                Value::Integer(id) => {
                    let ctx = ctx_root.borrow();
                    match ctx.editor().buffer(ctrlvim_types::BufferId(id.max(0) as u32)) {
                        Some(b) => b.name.clone().unwrap_or_default(),
                        None => return Ok(None),
                    }
                }
                _ => return Ok(None),
            };
            let names: Vec<String> = match markers {
                Value::String(s) => vec![s.to_str()?.to_string()],
                Value::Table(t) => t.sequence_values::<String>().collect::<mlua::Result<_>>()?,
                _ => Vec::new(),
            };
            let start_dir = if std::path::Path::new(&start_path).is_dir() {
                std::path::PathBuf::from(&start_path)
            } else {
                std::path::Path::new(&start_path).parent().map(Path::to_path_buf).unwrap_or_default()
            };
            let mut dir = Some(start_dir.as_path());
            while let Some(d) = dir {
                if names.iter().any(|n| d.join(n).exists()) {
                    return Ok(Some(d.to_string_lossy().into_owned()));
                }
                dir = d.parent();
            }
            Ok(None)
        })?;
        fs.set("root", root)?;

        vim.set("fs", fs)?;
        Ok(())
    }

    /// Polyfills for Lua 5.1 stdlib surface Neovim's runtime Lua still uses
    /// (Neovim bundles LuaJIT, which keeps `table.maxn`; real Lua 5.4, which
    /// `mlua`'s `lua54` feature gives us, dropped it).
    fn install_lua51_compat(&self) -> mlua::Result<()> {
        let table_tbl: Table = self.lua.globals().get("table")?;
        let maxn = self.lua.create_function(|_, t: Table| {
            let mut max: i64 = 0;
            for pair in t.pairs::<Value, Value>() {
                let (k, _) = pair?;
                let n = match k {
                    Value::Integer(i) => Some(i),
                    Value::Number(f) if f.fract() == 0.0 => Some(f as i64),
                    _ => None,
                };
                if let Some(n) = n {
                    max = max.max(n);
                }
            }
            Ok(max)
        })?;
        table_tbl.set("maxn", maxn)?;

        // Global `unpack` — Lua 5.1/LuaJIT built-in, renamed to
        // `table.unpack` in 5.4. Neovim's runtime Lua (written against
        // LuaJIT) uses the bare global throughout.
        let unpack: Function = table_tbl.get("unpack")?;
        self.lua.globals().set("unpack", unpack)?;

        Ok(())
    }

    /// Drain queued `vim.notify` messages as `(level, text)`.
    pub fn take_notices(&self) -> Vec<(i64, String)> {
        std::mem::take(&mut self.notices.borrow_mut())
    }

    /// Run everything queued with `vim.schedule`. The frontend calls this at a
    /// safe point in its loop — that deferral is the entire purpose of
    /// `vim.schedule`, so running the callbacks inline would defeat it.
    pub fn run_scheduled(&self) -> mlua::Result<usize> {
        let ids = std::mem::take(&mut *self.scheduled.borrow_mut());
        let count = ids.len();
        for id in ids {
            if let Some(func) = self.store.get(&self.lua, id)? {
                func.call::<_, ()>(())?;
            }
            self.store.remove(&self.lua, id)?;
        }
        Ok(count)
    }

    /// Number of live Lua callbacks held (for leak checks in tests).
    pub fn callback_count(&self) -> usize {
        self.store.len()
    }
}

/// Convert a Lua value to JSON, honoring the `vim._empty_dict_mt` marker (see
/// `Host::install_stdlib`) at any nesting depth — the reason this isn't just
/// `mlua::LuaSerdeExt::from_value` into `serde_json::Value` directly. A table
/// with a positive `#t` encodes as a JSON array (indices `1..#t`); otherwise,
/// as an object over its string-keyed pairs (or `{}` if there are none) —
/// Lua's own array/map ambiguity, resolved the same way real Neovim's
/// encoder resolves it.
fn lua_value_to_json(value: &Value, empty_dict_mt: &Table) -> mlua::Result<serde_json::Value> {
    Ok(match value {
        Value::Nil => serde_json::Value::Null,
        Value::Boolean(b) => serde_json::Value::Bool(*b),
        Value::Integer(i) => serde_json::Value::Number((*i).into()),
        Value::Number(n) => {
            serde_json::Number::from_f64(*n).map(serde_json::Value::Number).unwrap_or(serde_json::Value::Null)
        }
        Value::String(s) => serde_json::Value::String(s.to_str()?.to_string()),
        Value::Table(t) => {
            let is_empty_dict = t.get_metatable().is_some_and(|mt| mt == *empty_dict_mt);
            let len = t.raw_len();
            if !is_empty_dict && len > 0 {
                let mut arr = Vec::with_capacity(len);
                for i in 1..=len {
                    let v: Value = t.get(i)?;
                    arr.push(lua_value_to_json(&v, empty_dict_mt)?);
                }
                serde_json::Value::Array(arr)
            } else {
                let mut map = serde_json::Map::new();
                for pair in t.clone().pairs::<String, Value>() {
                    let (k, v) = pair?;
                    map.insert(k, lua_value_to_json(&v, empty_dict_mt)?);
                }
                serde_json::Value::Object(map)
            }
        }
        _ => serde_json::Value::Null,
    })
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
    fn poll_drains_both_timers_and_scheduled_callbacks_in_one_call() {
        let host = host_with("x");
        host.exec(
            r#"
            _G.timer_fired = false
            _G.scheduled_ran = false
            local t = vim.uv.new_timer()
            t:start(10, 0, function() _G.timer_fired = true end)
            vim.schedule(function() _G.scheduled_ran = true end)
            "#,
        )
        .unwrap();
        // Scheduled work never needs a timer to fire — it's a plain queue —
        // so a poll before the timer is due should still run it, matching
        // real Neovim (`vim.schedule` doesn't wait on anything).
        host.poll().unwrap();
        assert_eq!(host.eval_string("tostring(_G.scheduled_ran)").unwrap(), "true");

        // The timer itself needs the tokio side to actually fire, which a
        // non-blocking `poll()` alone doesn't wait for.
        std::thread::sleep(Duration::from_millis(50));
        host.poll().unwrap();
        assert_eq!(host.eval_string("tostring(_G.timer_fired)").unwrap(), "true");
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
    fn require_resolves_a_flat_module_under_a_runtime_path() {
        let dir = tempdir();
        std::fs::create_dir_all(dir.join("lua")).unwrap();
        std::fs::write(dir.join("lua/greeter.lua"), "return { greet = function() return 'hi' end }").unwrap();

        let host = host_with("x");
        host.add_runtime_path(dir.path());
        host.exec("_G.msg = require('greeter').greet()").unwrap();
        assert_eq!(host.eval_string("_G.msg").unwrap(), "hi");
    }

    #[test]
    fn require_resolves_a_nested_module_and_init_lua() {
        let dir = tempdir();
        std::fs::create_dir_all(dir.join("lua/pkg/sub")).unwrap();
        std::fs::write(dir.join("lua/pkg/init.lua"), "return { name = 'pkg' }").unwrap();
        std::fs::write(dir.join("lua/pkg/sub/mod.lua"), "return 42").unwrap();

        let host = host_with("x");
        host.add_runtime_path(dir.path());
        host.exec(
            r#"
            _G.name = require('pkg').name
            _G.num = require('pkg.sub.mod')
            "#,
        )
        .unwrap();
        assert_eq!(host.eval_string("_G.name").unwrap(), "pkg");
        assert_eq!(host.eval_string("_G.num").unwrap(), "42");
    }

    #[test]
    fn require_caches_a_module_so_it_runs_only_once() {
        let dir = tempdir();
        std::fs::create_dir_all(dir.join("lua")).unwrap();
        std::fs::write(dir.join("lua/counted.lua"), "_G.load_count = (_G.load_count or 0) + 1\nreturn true").unwrap();

        let host = host_with("x");
        host.add_runtime_path(dir.path());
        host.exec("require('counted'); require('counted')").unwrap();
        assert_eq!(host.eval_string("tostring(_G.load_count)").unwrap(), "1");
    }

    #[test]
    fn require_retries_after_a_failed_first_load_instead_of_caching_the_failure() {
        let dir = tempdir();
        std::fs::create_dir_all(dir.join("lua")).unwrap();
        std::fs::write(dir.join("lua/flaky.lua"), "if not _G.should_succeed then error('not yet') end\nreturn 'ok'").unwrap();

        let host = host_with("x");
        host.add_runtime_path(dir.path());
        assert!(host.exec("require('flaky')").is_err(), "first load should fail as written");

        // If the loading-guard placeholder leaked into the cache, this
        // second attempt would silently return `true` instead of retrying.
        host.exec("_G.should_succeed = true; _G.result = require('flaky')").unwrap();
        assert_eq!(host.eval_string("_G.result").unwrap(), "ok");
    }

    #[test]
    fn require_reports_a_missing_module_as_a_lua_error() {
        let host = host_with("x");
        let err = host.exec("require('does_not_exist')").unwrap_err().to_string();
        assert!(err.contains("not found"), "{err}");
    }

    #[test]
    fn require_checks_package_preload_before_the_filesystem() {
        let host = host_with("x");
        host.exec(
            r#"
            package.preload['seeded'] = function() return 'from preload' end
            _G.msg = require('seeded')
            "#,
        )
        .unwrap();
        assert_eq!(host.eval_string("_G.msg").unwrap(), "from preload");
    }

    /// A minimal stand-in for a real multi-file plugin like `nvim-lspconfig`:
    /// an `init.lua` that internally `require()`s a sibling module under the
    /// same runtime path, unmodified from how a real plugin would write it.
    #[test]
    fn require_supports_a_plugin_style_internal_require() {
        let dir = tempdir();
        std::fs::create_dir_all(dir.join("lua/demoplugin/configs")).unwrap();
        std::fs::write(
            dir.join("lua/demoplugin/init.lua"),
            "return { rust_analyzer = require('demoplugin.configs.rust_analyzer') }",
        )
        .unwrap();
        std::fs::write(
            dir.join("lua/demoplugin/configs/rust_analyzer.lua"),
            "return { cmd = { 'rust-analyzer' } }",
        )
        .unwrap();

        let host = host_with("x");
        host.add_runtime_path(dir.path());
        host.exec("_G.cmd = require('demoplugin').rust_analyzer.cmd[1]").unwrap();
        assert_eq!(host.eval_string("_G.cmd").unwrap(), "rust-analyzer");
    }

    /// A path added twice (e.g. declared in config *and* discovered under a
    /// pack directory) must not be searched twice or otherwise misbehave.
    #[test]
    fn add_runtime_path_is_idempotent() {
        let dir = tempdir();
        std::fs::create_dir_all(dir.join("lua")).unwrap();
        std::fs::write(dir.join("lua/once.lua"), "return true").unwrap();

        let host = host_with("x");
        host.add_runtime_path(dir.path());
        host.add_runtime_path(dir.path());
        assert_eq!(host.runtime_paths.borrow().len(), 1);
        host.exec("require('once')").unwrap();
    }

    /// A unique-per-test directory under the system temp dir, cleaned up on
    /// drop — `require()` reads real files, so these tests need real paths.
    fn tempdir() -> TempDir {
        TempDir::new()
    }

    struct TempDir(PathBuf);
    impl TempDir {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!("ctrlvim-lua-test-{}", uid()));
            std::fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }
        fn join(&self, p: &str) -> PathBuf {
            self.0.join(p)
        }
        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }
    impl std::ops::Deref for TempDir {
        type Target = PathBuf;
        fn deref(&self) -> &PathBuf {
            &self.0
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn uid() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let pid = std::process::id() as u64;
        pid << 32 | NEXT.fetch_add(1, Ordering::Relaxed)
    }

    /// Unmodified-luv-style usage: create the three stdio pipes, spawn with
    /// them, write to stdin, and read the child's stdout back through
    /// `read_start` — the exact shape `vim.lsp.rpc.lua` uses to talk to a
    /// language server, minus the LSP framing itself.
    #[test]
    fn vim_uv_spawn_round_trips_stdio_through_pipes() {
        let host = host_with("x");
        host.exec(
            r#"
            _G.out = {}
            _G.exited = nil
            local stdin = vim.uv.new_pipe(false)
            local stdout = vim.uv.new_pipe(false)
            local stderr = vim.uv.new_pipe(false)
            local handle, job_id = vim.uv.spawn('cat', { args = {}, stdio = { stdin, stdout, stderr } }, function(code, signal)
                _G.exited = { code = code, signal = signal }
            end)
            _G.handle = handle
            _G.job_id = job_id
            stdout:read_start(function(err, chunk)
                if chunk then
                    table.insert(_G.out, chunk)
                end
            end)
            stdin:write('hello ')
            stdin:write('uv')
            stdin:close()
            "#,
        )
        .unwrap();
        assert!(host.eval_string("tostring(_G.job_id)").unwrap().parse::<u64>().is_ok());

        // Drain until the process exits, same loop the frontend runs.
        for _ in 0..50 {
            if host.eval_string("tostring(_G.exited ~= nil)").unwrap() == "true" {
                break;
            }
            host.run_events(Duration::from_secs(2)).unwrap();
        }
        assert_eq!(host.eval_string("tostring(_G.exited.code)").unwrap(), "0");
        host.exec("_G.joined = table.concat(_G.out)").unwrap();
        assert_eq!(host.eval_string("_G.joined").unwrap(), "hello uv");
    }

    #[test]
    fn vim_uv_spawn_keeps_stdout_and_stderr_on_separate_pipes() {
        let host = host_with("x");
        host.exec(
            r#"
            _G.out, _G.err = {}, {}
            local stdin = vim.uv.new_pipe(false)
            local stdout = vim.uv.new_pipe(false)
            local stderr = vim.uv.new_pipe(false)
            vim.uv.spawn('sh', { args = { '-c', 'cat; echo oops >&2' }, stdio = { stdin, stdout, stderr } }, function() end)
            stdout:read_start(function(_, chunk) if chunk then table.insert(_G.out, chunk) end end)
            stderr:read_start(function(_, chunk) if chunk then table.insert(_G.err, chunk) end end)
            stdin:write('payload')
            stdin:close()
            "#,
        )
        .unwrap();

        for _ in 0..50 {
            host.exec("_G.err_joined = table.concat(_G.err)").unwrap();
            if host.eval_string("_G.err_joined").unwrap().contains("oops") {
                break;
            }
            host.run_events(Duration::from_secs(2)).unwrap();
        }
        host.exec("_G.out_joined = table.concat(_G.out)").unwrap();
        assert_eq!(host.eval_string("_G.out_joined").unwrap(), "payload");
        assert_eq!(host.eval_string("_G.err_joined").unwrap().trim(), "oops");
    }

    #[test]
    fn vim_uv_fs_stat_reports_directories_and_missing_paths() {
        let host = host_with("x");
        host.exec(
            r#"
            local stat = vim.uv.fs_stat('.')
            _G.is_dir = stat and stat.type == 'directory'
            local missing = vim.uv.fs_stat('/no/such/path/ctrlvim-test')
            _G.missing_is_nil = missing == nil
            "#,
        )
        .unwrap();
        assert_eq!(host.eval_string("tostring(_G.is_dir)").unwrap(), "true");
        assert_eq!(host.eval_string("tostring(_G.missing_is_nil)").unwrap(), "true");
    }

    #[test]
    fn vendored_vim_lsp_and_diagnostic_load_without_error() {
        let host = host_with("x");
        host.load_vendored_lsp_runtime().unwrap();
        assert_eq!(host.eval_string("type(vim.lsp)").unwrap(), "table");
        assert_eq!(host.eval_string("type(vim.lsp.start)").unwrap(), "function");
        assert_eq!(host.eval_string("type(vim.diagnostic)").unwrap(), "table");
        assert_eq!(host.eval_string("type(vim.uri_from_fname)").unwrap(), "function");
    }

    #[test]
    fn vendored_vim_lsp_protocol_builds_client_capabilities() {
        let host = host_with("x");
        host.load_vendored_lsp_runtime().unwrap();
        host.exec("_G.caps = vim.lsp.protocol.make_client_capabilities()").unwrap();
        assert_eq!(host.eval_string("type(_G.caps)").unwrap(), "table");
        assert_eq!(host.eval_string("type(_G.caps.textDocument)").unwrap(), "table");
    }

    #[test]
    fn vendored_vim_diagnostic_set_and_get_round_trip() {
        let host = host_with("line one\nline two");
        host.load_vendored_lsp_runtime().unwrap();
        host.exec(
            r#"
            local ns = vim.api.nvim_create_namespace('test')
            local bufnr = vim.api.nvim_get_current_buf()
            vim.diagnostic.set(ns, bufnr, {
                {
                    lnum = 0,
                    col = 0,
                    end_lnum = 0,
                    end_col = 4,
                    severity = vim.diagnostic.severity.ERROR,
                    message = 'something is wrong',
                    source = 'test',
                },
            })
            _G.diags = vim.diagnostic.get(bufnr)
            "#,
        )
        .unwrap();
        assert_eq!(host.eval_string("#_G.diags").unwrap(), "1");
        assert_eq!(host.eval_string("_G.diags[1].message").unwrap(), "something is wrong");
        assert_eq!(host.eval_string("_G.diags[1].lnum").unwrap(), "0");
    }

    /// A minimal stand-in for a real `nvim-lspconfig` checkout's `lsp/`
    /// preset directory (`lsp/rust_analyzer.lua`, `lsp/*.lua` glob support),
    /// which is how modern lspconfig actually ships server configs — not
    /// `require()`, but Neovim's separate `'runtimepath'`-file convention.
    /// Verified against the real file's shape manually (see the LSP
    /// vendoring notes); this fixture is small and self-contained so the
    /// regression test doesn't need network access or a real checkout.
    /// The real end-to-end proof: spawn an actual `rust-analyzer` (via real
    /// `vim.uv.spawn`), speak real LSP JSON-RPC to it (via real
    /// `vim.lsp.rpc`), and complete the `initialize` handshake — using the
    /// real `nvim-lspconfig` v2.11.0 `lsp/rust_analyzer.lua` preset,
    /// unmodified, from a real checkout. Requires `rust-analyzer` on `PATH`
    /// and network access to have fetched the checkout — not something CI
    /// should depend on, hence `#[ignore]`; run manually with
    /// `cargo test -p ctrlvim-lua -- --ignored real_rust_analyzer`.
    #[test]
    #[ignore = "needs a real rust-analyzer on PATH and a real nvim-lspconfig checkout — manual verification only"]
    fn end_to_end_real_rust_analyzer_via_real_lspconfig_preset() {
        let lspconfig_dir = std::env::var("CTRLVIM_TEST_LSPCONFIG_DIR")
            .expect("set CTRLVIM_TEST_LSPCONFIG_DIR to a real nvim-lspconfig checkout");
        let project_dir = std::env::var("CTRLVIM_TEST_RUST_PROJECT_DIR")
            .expect("set CTRLVIM_TEST_RUST_PROJECT_DIR to a real cargo project (e.g. this repo)");

        let host = host_with("fn main() {}\n");
        host.add_runtime_path(lspconfig_dir);
        host.load_vendored_lsp_runtime().unwrap();
        host.exec("vim.lsp.log.set_level('trace')").unwrap();
        host.exec(&format!(
            r#"
            local cfg = vim.lsp.config.rust_analyzer
            local ok, id_or_err = pcall(vim.lsp.start, {{
                name = 'rust_analyzer',
                cmd = cfg.cmd,
                root_dir = {project_dir:?},
            }})
            _G.start_ok = ok
            if ok then
                _G.client_id = id_or_err
            else
                _G.start_err = tostring(id_or_err)
            end
            "#
        ))
        .unwrap();
        if host.eval_string("tostring(_G.start_ok)").unwrap() != "true" {
            panic!("vim.lsp.start errored: {}", host.eval_string("_G.start_err").unwrap());
        }
        if host.eval_string("_G.client_id ~= nil").unwrap() != "true" {
            panic!("vim.lsp.start returned nil; vim.notify said: {:?}", host.take_notices());
        }

        let mut initialized = false;
        for _ in 0..300 {
            host.run_events(Duration::from_millis(200)).unwrap();
            // `vim.lsp.rpc` defers actual message handling via
            // `vim.schedule` (matching real Neovim's fast-event-safety
            // pattern) — draining `run_events` alone leaves it queued
            // forever. Real Neovim's main loop runs both every tick; so must
            // this poll.
            host.run_scheduled().unwrap();
            let done = host
                .eval_string("tostring(vim.lsp.get_client_by_id(_G.client_id) and vim.lsp.get_client_by_id(_G.client_id).initialized or false)")
                .unwrap();
            if done == "true" {
                initialized = true;
                break;
            }
        }
        assert!(initialized, "rust-analyzer never completed the LSP initialize handshake");

        host.exec(
            r#"
            local client = vim.lsp.get_client_by_id(_G.client_id)
            _G.has_hover = client.server_capabilities.hoverProvider ~= nil
            _G.has_definition = client.server_capabilities.definitionProvider ~= nil
            "#,
        )
        .unwrap();
        assert_eq!(host.eval_string("tostring(_G.has_hover)").unwrap(), "true");
        assert_eq!(host.eval_string("tostring(_G.has_definition)").unwrap(), "true");

        // Separate call: a `client:stop()` hiccup shouldn't be able to mask
        // the capability assertions above having already passed.
        host.exec("vim.lsp.get_client_by_id(_G.client_id):stop()").unwrap();
    }

    #[test]
    fn vim_lsp_config_resolves_a_runtimepath_lsp_preset_file() {
        let dir = tempdir();
        std::fs::create_dir_all(dir.join("lsp")).unwrap();
        std::fs::write(
            dir.join("lsp/rust_analyzer.lua"),
            "return { cmd = { 'rust-analyzer' }, filetypes = { 'rust' }, root_markers = { 'Cargo.toml' } }",
        )
        .unwrap();

        let host = host_with("x");
        host.add_runtime_path(dir.path());
        host.load_vendored_lsp_runtime().unwrap();
        host.exec(
            r#"
            local cfg = vim.lsp.config.rust_analyzer
            _G.cmd = cfg.cmd[1]
            _G.filetype = cfg.filetypes[1]
            "#,
        )
        .unwrap();
        assert_eq!(host.eval_string("_G.cmd").unwrap(), "rust-analyzer");
        assert_eq!(host.eval_string("_G.filetype").unwrap(), "rust");
    }

    #[test]
    fn nvim_get_runtime_file_collects_matches_across_multiple_roots_when_all_is_true() {
        let dir1 = tempdir();
        let dir2 = tempdir();
        std::fs::create_dir_all(dir1.join("lsp")).unwrap();
        std::fs::create_dir_all(dir2.join("lsp")).unwrap();
        std::fs::write(dir1.join("lsp/foo.lua"), "return 1").unwrap();
        std::fs::write(dir2.join("lsp/bar.lua"), "return 2").unwrap();

        let host = host_with("x");
        host.add_runtime_path(dir1.path());
        host.add_runtime_path(dir2.path());
        host.exec(
            r#"
            _G.all = vim.api.nvim_get_runtime_file('lsp/*.lua', true)
            _G.first_only = vim.api.nvim_get_runtime_file('lsp/foo.lua', false)
            "#,
        )
        .unwrap();
        assert_eq!(host.eval_string("#_G.all").unwrap(), "2");
        assert_eq!(host.eval_string("#_G.first_only").unwrap(), "1");
    }

    #[test]
    fn nvim_exec_autocmds_invokes_matching_lua_callbacks_now() {
        let host = host_with("x");
        host.exec(
            r#"
            _G.fired = {}
            vim.api.nvim_create_autocmd('User', {
                pattern = 'MyEvent',
                callback = function(ev) table.insert(_G.fired, ev.event .. ':' .. ev.file) end,
            })
            vim.api.nvim_exec_autocmds('User', { pattern = 'MyEvent' })
            "#,
        )
        .unwrap();
        assert_eq!(host.eval_string("_G.fired[1]").unwrap(), "User:MyEvent");
    }

    #[test]
    fn nvim_buf_attach_fires_on_lines_after_a_lua_driven_edit() {
        let host = host_with("a\nb\nc");
        host.exec(
            r#"
            _G.calls = {}
            local buf = vim.api.nvim_get_current_buf()
            vim.api.nvim_buf_attach(buf, false, {
                on_lines = function(tag, bufnr, tick, firstline, lastline, new_lastline)
                    table.insert(_G.calls, { tag, firstline, lastline, new_lastline })
                end,
            })
            vim.api.nvim_buf_set_lines(buf, 1, 2, false, { 'B' })
            "#,
        )
        .unwrap();
        // Nothing fires until the embedder checks — this is the documented
        // "driven by the embedder, not automatic" contract.
        assert_eq!(host.eval_string("#_G.calls").unwrap(), "0");

        host.notify_buf_lines_changed(ctrlvim_types::BufferId(1)).unwrap();
        assert_eq!(host.eval_string("#_G.calls").unwrap(), "1");
        assert_eq!(host.eval_string("_G.calls[1][1]").unwrap(), "lines");
        assert_eq!(host.eval_string("_G.calls[1][2]").unwrap(), "1");
        assert_eq!(host.eval_string("_G.calls[1][3]").unwrap(), "2");
        assert_eq!(host.eval_string("_G.calls[1][4]").unwrap(), "2");

        // No further change: a second check fires nothing.
        host.notify_buf_lines_changed(ctrlvim_types::BufferId(1)).unwrap();
        assert_eq!(host.eval_string("#_G.calls").unwrap(), "1");
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
    fn vim_keymap_set_with_a_string_rhs_defines_a_real_mapping() {
        // The shape almost every Neovim config uses. A callback still needs the
        // typeahead layer to dispatch it, but a string rhs is just a mapping.
        let host = host_with("x");
        host.exec("vim.keymap.set('n', '<leader>g', ':Find<CR>', { desc = 'grep' })").unwrap();
        let ctx = host.ctx.borrow();
        let maps = ctx.session.keymap.list(ctrlvim_editor::keymap::MapMode::Normal);
        let m = maps.iter().find(|m| m.lhs_notation() == "<Space>g").expect("mapping stored");
        assert_eq!(m.rhs_notation(), ":Find<CR>");
        assert_eq!(m.desc.as_deref(), Some("grep"));
    }

    #[test]
    fn vim_keymap_set_keeps_a_callbacks_desc() {
        let host = host_with("x");
        host.exec("vim.keymap.set('n', '<leader>y', function() end, { desc = 'do a thing' })")
            .unwrap();
        let ctx = host.ctx.borrow();
        let (_, desc) = &ctx.keymaps[&("n".to_string(), "<leader>y".to_string())];
        assert_eq!(desc.as_deref(), Some("do a thing"));
    }

    #[test]
    fn vim_keymap_set_rejects_a_rhs_that_is_neither_string_nor_function() {
        let host = host_with("x");
        let e = host.exec("vim.keymap.set('n', 'x', 42)").unwrap_err().to_string();
        assert!(e.contains("must be a string or a function"), "{e}");
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

#[cfg(test)]
mod stdlib_tests {
    use super::*;

    fn host() -> Host {
        Host::new(Editor::new()).unwrap()
    }

    #[test]
    fn vim_opt_reads_and_writes_options() {
        let h = host();
        h.exec("assert(vim.opt.number == false)").unwrap();
        h.exec("vim.opt.number = true").unwrap();
        h.exec("assert(vim.opt.number == true, 'number should be on')")
            .unwrap();
        // `vim.o` is the same proxy.
        h.exec("assert(vim.o.number == true)").unwrap();
        // Numeric and string options round-trip too.
        h.exec("vim.opt.tabstop = 2").unwrap();
        h.exec("assert(vim.opt.tabstop == 2, 'tabstop should be 2')")
            .unwrap();
        h.exec("vim.opt.foldmethod = 'indent'").unwrap();
        h.exec("assert(vim.opt.foldmethod == 'indent')").unwrap();
    }

    #[test]
    fn writing_an_option_goes_through_the_engines_set() {
        let h = host();
        h.exec("vim.opt.number = false").unwrap();
        // `false` must become `:set nonumber`, not `:set number=false`.
        h.exec("assert(vim.opt.number == false)").unwrap();
    }

    #[test]
    fn vim_g_stores_globals() {
        let h = host();
        h.exec("assert(vim.g.missing == nil, 'unset globals read as nil')")
            .unwrap();
        h.exec("vim.g.mapleader = ' '").unwrap();
        h.exec("assert(vim.g.mapleader == ' ')").unwrap();
        h.exec("vim.g.count = 7").unwrap();
        h.exec("assert(vim.g.count == 7)").unwrap();
    }

    #[test]
    fn vim_g_is_shared_with_vimscript() {
        let h = host();
        h.exec("vim.g.shared = 42").unwrap();
        // The same variable is visible as `g:shared` to the interpreter.
        h.exec("assert(vim.fn.string(vim.g.shared) == '42')").unwrap();
    }

    #[test]
    fn vim_cmd_runs_ex_commands() {
        let h = host();
        h.exec("vim.cmd('set number')").unwrap();
        h.exec("assert(vim.opt.number == true, 'vim.cmd ran :set')")
            .unwrap();
        // A leading colon is accepted too.
        h.exec("vim.cmd(':set nonumber')").unwrap();
        h.exec("assert(vim.opt.number == false)").unwrap();
    }

    #[test]
    fn vim_notify_queues_messages_for_the_frontend() {
        let h = host();
        h.exec("vim.notify('hello')").unwrap();
        h.exec("vim.notify('bad', 4)").unwrap();
        let notices = h.take_notices();
        assert_eq!(notices.len(), 2);
        assert_eq!(notices[0].1, "hello");
        assert_eq!(notices[1], (4, "bad".to_string()));
        assert!(h.take_notices().is_empty(), "draining clears the queue");
    }

    #[test]
    fn vim_schedule_defers_until_run() {
        let h = host();
        h.exec("ran = false; vim.schedule(function() ran = true end)")
            .unwrap();
        h.exec("assert(ran == false, 'must not run inline')").unwrap();
        assert_eq!(h.run_scheduled().unwrap(), 1);
        h.exec("assert(ran == true, 'runs when drained')").unwrap();
        // The queue is empty afterwards, and the callback ref was released.
        assert_eq!(h.run_scheduled().unwrap(), 0);
    }

    #[test]
    fn string_and_table_helpers() {
        let h = host();
        h.exec("local p = vim.split('a,b,c', ','); assert(#p == 3 and p[2] == 'b')")
            .unwrap();
        h.exec("assert(vim.trim('  x  ') == 'x')").unwrap();
        h.exec("assert(vim.startswith('foobar', 'foo'))").unwrap();
        h.exec("assert(vim.endswith('foobar', 'bar'))").unwrap();
        h.exec("assert(vim.tbl_count({a=1, b=2}) == 2)").unwrap();
        h.exec("assert(vim.tbl_isempty({}))").unwrap();
        h.exec("assert(not vim.tbl_isempty({1}))").unwrap();
        h.exec("assert(#vim.tbl_keys({a=1, b=2}) == 2)").unwrap();
        h.exec("assert(#vim.tbl_values({a=1, b=2}) == 2)").unwrap();
    }

    #[test]
    fn tbl_extend_honors_force_and_keep() {
        let h = host();
        h.exec("local m = vim.tbl_extend('force', {a=1}, {a=2, b=3}); assert(m.a == 2 and m.b == 3)")
            .unwrap();
        h.exec("local m = vim.tbl_extend('keep', {a=1}, {a=2, b=3}); assert(m.a == 1 and m.b == 3)")
            .unwrap();
    }
}
