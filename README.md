# ctrlvim-rs

A staged Rust reimplementation of Neovim's editing core, with the UI deliberately
omitted (the frontend is built separately in [Ratatui]) and existing Neovim Lua
plugins as the long-term compatibility target. See the roadmap in
`~/.claude/plans/vivid-humming-elephant.md` for the full plan.

This tree covers **milestones M0–M9**. Everything here compiles on stable Rust and
is covered by tests (`cargo test --workspace` — 138 tests, zero warnings).

## Quick start

```sh
cargo test --workspace                       # run all tests
cargo run -p ctrlvim-core --bin ctrlvim-demo    # end-to-end M1–M5 demo (no UI)
```

## Workspace layout

| crate | replaces (Neovim C) | what it does |
|-------|---------------------|--------------|
| `ctrlvim-types` | `Object`/`typval`, handles | dynamic `Object` value, `BufferId`/`WindowId` arena handles, `Position`/`Range`, errors |
| `ctrlvim-text` | `memline.c`, `mark.c`+`marktree.c`+`extmark.c`, `undo.c`, `register.c` | rope-backed `Buffer`, unified `MarkStore`, arena `UndoTree` (with `g-`/`g+`), `Registers` |
| `ctrlvim-options` | `option.c` + `options.lua` | three-tier (global/buffer/window) options via `Option<T>` overrides |
| `ctrlvim-editor` | `normal.c`, `ops.c`, `textobject.c`, `edit.c`, `state.c`, `window.c` (model) | `Editor` context (no globals), motions, operators, `Mode` state machine, split `Frame` tree + `<C-w>` window cmds, `Session` key dispatch |
| `ctrlvim-vimscript` | `eval.c` + `eval/*.c`, `eval.lua` builtins | minimal Vimscript interpreter (`let`/`if`/`for`/`while`/`function`), `vim.fn.*` builtins |
| `ctrlvim-treesitter` | `lua/treesitter.c` | binding surface over the `tree-sitter` crate: parse, query, node ranges |
| `ctrlvim-api-macro` | `gen_api_dispatch.lua` + `c_grammar.lua` | `#[ctrlvim_api]` proc-macro → generates Lua + RPC dispatch, no text parsing |
| `ctrlvim-api` | `src/ctrlvim/api/*.c` | `ApiContext` (owns the `Session`), `#[ctrlvim_api]` functions, `inventory` registry, autocmd store, `vim.fn` bridge |
| `ctrlvim-lua` | `executor.c` + `converter.c` | `mlua` embedding, `Object`↔Lua converter, `LuaRef`/`RegistryKey` callbacks, `vim.api`/`vim.uv`/`vim.fn`/`vim.keymap`/`vim.treesitter` |
| `ctrlvim-async` | `event/*.c` (libuv), `msgpack_rpc/*.c` | tokio event loop + timers, `rmpv` msgpack-RPC codec/dispatch |
| `ctrlvim-core` | startup wiring | `Ctrlvim` facade + demo binary a frontend links against |

Dependency direction flows strictly downward; `ctrlvim-async` is a parallel infra branch.

## Milestone status

- **M0 — spikes** ✅ verified `mlua` (vendored Lua 5.4) + `tokio` build here; proc-macro,
  rope buffer, and `RegistryKey` callback round-trip all proven.
- **M1 — buffer + motions** ✅ rope `Buffer`, `Editor` with arena handles, `hjkl`/`w`/`b`/`e`/
  `0`/`^`/`$`/`gg`/`G` motions with counts, char-class word logic.
- **M2 — marks/undo/registers** ✅ unified marktree with gravity, undo *tree* with `g-`/`g+`
  branch traversal, register ring (`0`, `1`–`9`, named, `-`, blackhole), operator framework
  (`d`/`y`/`c`) shared by Normal-motion and Visual selection.
- **M3 — modal editing** ✅ Normal/Insert/Visual(char+line)/Cmdline modes, `i`/`a`/`I`/`A`/`o`/`O`,
  `x`, `p`/`P`, `u`/`<C-r>`, minimal `:` commands. *Deferred:* `getchar`/`:map` typeahead,
  search motions/regex, blockwise visual, options `build.rs` codegen (hand-written for now).
- **M4 — Lua core** ✅ real `mlua` runtime; `vim.api.*` calls dispatch through the same
  registry as RPC; `Object`↔Lua conversion (array/dict/function→`LuaRef`); Lua autocmd
  callbacks fire via the shared `LuaRef` mechanism. *Deferred:* vendoring `runtime/lua/vim/**`,
  full `vim.fn.*`.
- **M5 — async I/O** ✅ tokio-backed `EventLoop`+`TimerService`; `vim.uv.new_timer():start()`
  fires real Lua callbacks; `vim.loop` alias; msgpack-RPC codec + dispatch. *Deferred:*
  `vim.uv.spawn` (process I/O), socket/stdio channel transports.
- **M6 — Vimscript + vim.fn** ✅ tree-walking interpreter (scopes `g:`/`l:`/`a:`/`v:`,
  arithmetic/string/comparison/logical exprs, lists/dicts, `if`/`for`/`while`, user
  `function`s with recursion), ~40 `vim.fn` builtins, wired as `vim.fn.*`/`vim.call`
  over the shared editor. *Deferred:* `:execute`, exceptions, autoload, closures, regex.
- **M7 — treesitter** ✅ `tree-sitter`-crate-backed parse + query + node-range extraction;
  `vim.treesitter.query`/`root_kind` from Lua; JSON grammar wired for the demo. *Deferred:*
  full `TSNode`/`TSTree` userdata surface, injections, incremental reparse.
- **M8 — plugin integration** ✅ `Session` + Lua share one editor; `vim.keymap.set`;
  a plugin-style Lua script drives api + fn + autocmd + keymap + treesitter end-to-end,
  with interactive keys hitting the same buffer. *Deferred:* vendoring real
  `ctrlvim-treesitter`/`lspconfig` (needs `runtime/lua/vim/**` + a live LSP server).
- **M9 — windows/splits** ✅ split `Frame` tree, `<C-w>s`/`v`/`w`/`q` commands, window
  cycle/close, `ctrlvim_list_wins`/`ctrlvim_get_current_win`/`ctrlvim_split_window`, and a
  `layout_rects(w,h)` model query a Ratatui frontend renders from.

## Design notes

- **No global `curbuf`/`curwin`.** Everything threads through an explicit `Editor`, with
  `Copy` integer handles instead of raw pointers — a stale handle is a clean `None`.
- **One callback mechanism.** Autocmds, and (next) keymaps/timers all store an
  `Object::LuaRef` resolved through `LuaRefStore` (mlua `RegistryKey`) — the Rust twin of
  `nlua_call_ref`.
- **Codegen is a proc-macro, not a C-text parser.** `#[ctrlvim_api]` on an ordinary Rust fn
  emits both dispatch paths; `inventory` auto-collects them at link time.
