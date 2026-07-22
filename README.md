# ctrlvim

A Rust reimplementation of Neovim's editing core with a Ratatui frontend.

## Features

- **Buffer Engine** — Rope-backed buffers with unified mark tree, undo tree (with `g-`/`g+` branch traversal), and register ring
- **Modal Editing** — Normal/Insert/Visual/Cmdline modes with motions, operators, and text objects
- **Lua Runtime** — `mlua`-based Lua embedding with `vim.api.*`, `vim.fn.*`, `vim.keymap`, and `vim.treesitter`
- **Async I/O** — Tokio-backed event loop with timers and msgpack-RPC codec
- **Vimscript** — Tree-walking interpreter supporting `let`/`if`/`for`/`while`/`function`
- **Treesitter** — Parse, query, and node-range extraction via `tree-sitter` crate
- **Window Management** — Split frame tree with `<C-w>` commands
- **TUI Frontend** — Ratatui + crossterm dashboard, plugin manager, file browser, and floating overlays

## Quick start

```sh
cargo test --workspace                            # run all tests
cargo run -p ctrlvim-core --bin ctrlvim-demo       # end-to-end demo (no UI)
cargo run -p ctrlvim-tui                           # launch the TUI
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
| `ctrlvim-tui` | (the UI, split out) | Ratatui + crossterm frontend: startup dashboard, plugin manager, file-buffer viewer, and floating explorer/palette/help overlays, keyboard + mouse driven, atop `ctrlvim-core` |

Dependency direction flows strictly downward; `ctrlvim-async` is a parallel infra branch.

## Design notes

- **No global `curbuf`/`curwin`.** Everything threads through an explicit `Editor`, with `Copy` integer handles instead of raw pointers — a stale handle is a clean `None`.
- **One callback mechanism.** Autocmds, keymaps, and timers all store an `Object::LuaRef` resolved through `LuaRefStore` (mlua `RegistryKey`) — the Rust twin of `nlua_call_ref`.
- **Codegen is a proc-macro, not a C-text parser.** `#[ctrlvim_api]` on an ordinary Rust fn emits both dispatch paths; `inventory` auto-collects them at link time.

## Changelog

### [0.1.0] - 2026-07-22

#### Added
- Motion operators (`d`, `y`, `c`), visual mode (char + line), and text objects
- TUI improvements including floating overlays and mouse support

### [0.1.0] - 2026-07-20

#### Added
- `ctrlvim-tui` crate with Ratatui + crossterm frontend
- `ctrlvim-markdown` crate for markdown support
- Startup dashboard, plugin manager, and file browser UI

#### Fixed
- `.gitignore` patterns for IDE files and OS artifacts

### [0.1.0] - 2026-07-10

#### Added
- Initial project structure with all core crates
- Rope-backed buffer engine with mark tree, undo tree, and registers
- Modal editing (Normal/Insert/Visual/Cmdline) with motions and operators
- Lua runtime with `vim.api.*`, `vim.fn.*`, `vim.keymap`, and `vim.treesitter`
- Tokio async event loop with timers and msgpack-RPC
- Vimscript interpreter with `let`/`if`/`for`/`while`/`function`
- Treesitter integration for parse, query, and node ranges
- Window management with split frames and `<C-w>` commands
- Proc-macro for API dispatch generation

#### Changed
- Renamed all `nvim` references to `ctrlvim` across the project
