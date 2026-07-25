# ctrlvim / cvi

**ctrlvim** is a modern text editor built from scratch in Rust, designed as a faithful reimplementation of Neovim's editing model. **cvi** is its terminal UI, built with [Ratatui](https://github.com/ratatui/ratatui).

If you love Vim, but have ever wished for an editor that could be extended with Rust as easily as Lua, this is for you.

## Why?

Neovim is incredible — but its C codebase is a mountain of complexity. ctrlvim reimagines the core from the ground up in memory-safe Rust, preserving what makes Vim great (modal editing, extensibility, terminal-native feel) while making the internals approachable and hackable.

## Features

- **Modal editing that just works** — Normal, Insert, Visual, and Command-line modes with motions (`w`, `b`, `e`, `{`, `}`), operators (`d`, `y`, `c`), and text objects (`iw`, `a"`, `i(`)
- **Lua plugins** — Full `vim.api.*`, `vim.fn.*`, `vim.keymap`, and `vim.treesitter` compatibility via `mlua`
- **Tree-sitter** — Syntax-aware parsing and code navigation built in, driving live syntax highlighting in the editor (Rust and JSON so far)
- **Undo tree** — Branch-aware undo/redo (`g-` / `g+`) that doesn't lose history
- **Registers** — Yank ring, named registers, clipboard integration
- **Window management** — Splits with `<C-w>` commands, just like Vim
- **Async I/O** — Tokio-powered event loop for timers, and job control that streams a spawned program's output into the editor without blocking it
- **Quickfix list** — `:vimgrep`, `:make`, and `:grep` fill a navigable list (`:copen`, `:cnext`) that jumps straight to the file and line
- **Folds** — `zf`/`za`/`zR`/`zM` and `foldmethod=indent`, with fold-aware movement and scrolling
- **Tags** — `Ctrl-]` / `Ctrl-T` and the `:tag` family over a `ctags -R .` table
- **TUI interface** — Dashboard, file browser, plugin manager, and floating overlays
- **Nerd Font file icons** — Per-filetype glyphs in the dashboard and file explorer, falling back to the lettered chip when no Nerd Font is installed

## Quick start

```sh
cargo run -p ctrlvim           # launch the editor
cargo run -p ctrlvim-core      # headless demo (no UI)
cargo test --workspace         # run all tests
```

## Architecture

The project is organized into focused crates, each handling one concern:

| Crate | Purpose |
|-------|---------|
| `ctrlvim-text` | Rope-backed buffers, marks, undo tree, registers |
| `ctrlvim-editor` | Motions, operators, text objects, window splits |
| `ctrlvim-lua` | Lua embedding and `vim.*` API compatibility |
| `ctrlvim-api` | `#[ctrlvim_api]` dispatch generation |
| `ctrlvim-treesitter` | Tree-sitter integration + the `highlights.scm` → styled-span highlighter |
| `ctrlvim-async` | Tokio event loop and msgpack-RPC |
| `ctrlvim-vimscript` | Vimscript interpreter |
| `ctrlvim-tui` | Terminal UI (Ratatui + crossterm) |

## License

Apache-2.0