# ctrlvim / cvi

**ctrlvim** is a modern text editor built from scratch in Rust, designed as a faithful reimplementation of Neovim's editing model. **cvi** is its terminal UI, built with [Ratatui](https://github.com/ratatui/ratatui).

If you love Vim, but have ever wished for an editor that could be extended with Rust as easily as Lua, this is for you.

## Why?

Neovim is incredible — but its C codebase is a mountain of complexity. ctrlvim reimagines the core from the ground up in memory-safe Rust, preserving what makes Vim great (modal editing, extensibility, terminal-native feel) while making the internals approachable and hackable.

## Features

- **Modal editing that just works** — Normal, Insert, Visual, and Command-line modes with motions (`w`, `b`, `e`, `{`, `}`), operators (`d`, `y`, `c`), and text objects (`iw`, `a"`, `i(`). Counts compose the way Vim's do, so `2d3w` deletes six words
- **TOML configuration** — `~/.config/ctrlvim/config.toml` declares options, per-mode keymaps, autocommands, and plugins. No config script to write; see [`docs/config.example.toml`](docs/config.example.toml)
- **Macros** — `q{reg}` records, `@{reg}` replays, `@@` repeats. Stored as ordinary register text, so a macro can be pasted out, edited, and yanked back
- **Marks & jumps** — `m{a-z}`, `` `{mark} ``, `'{mark}`, addressable in Ex ranges (`:'a,'bd`), plus a real jumplist on `<C-o>`/`<C-i>`
- **A real Vim regex engine** — written from scratch rather than translated onto a general-purpose regex library, so the constructs a DFA cannot express all work: backreferences (`\1`), lookaround (`\@=`, `\@!`, `\@<=`, `\@<!`), atomic groups (`\@>`), match-boundary markers (`\zs`/`\ze`), non-greedy repeats (`\{-n,m}`), all four magic levels (`\v` `\m` `\M` `\V`), positional atoms (`\%23l`, `\%>4v`), and search offsets (`/pat/e+1`). `'ignorecase'` and `'smartcase'` apply everywhere patterns do
- **Lua plugins** — `vim.api.*`, `vim.fn.*`, `vim.opt`/`vim.o`, `vim.g`, `vim.cmd`, `vim.keymap`, `vim.notify`, `vim.schedule`, the `vim.tbl_*` helpers, and `vim.treesitter`, via `mlua`
- **msgpack-RPC server** — an external client can attach over a Unix socket and drive the editor through the same API surface Lua uses
- **Tree-sitter** — Syntax-aware parsing and code navigation built in, driving live syntax highlighting in the editor (Rust and JSON so far)
- **Undo tree** — Branch-aware undo/redo (`g-` / `g+`) that doesn't lose history
- **Registers** — Yank ring, named registers, clipboard integration
- **Window management** — `:split`/`:vsplit` and `<C-w>` commands including directional focus (`<C-w>h/j/k/l`), honoring `'splitbelow'`/`'splitright'`
- **Async I/O** — Tokio-powered event loop for timers, and job control that streams a spawned program's output into the editor without blocking it
- **Quickfix list** — `:vimgrep`, `:make`, and `:grep` fill a navigable list (`:copen`, `:cnext`) that jumps straight to the file and line
- **Find & replace across the project** — `<leader>S` (or `:Find`) opens a live panel seeded with the word under the cursor: every match grouped by file, a before/after diff of the line each one would become, and `y`/`Y` to accept one or all. Same engine as `:s`, so `\<word\>`, `\(groups\)`, `\1` and `\U\1` all carry over
- **Folds** — `zf`/`za`/`zR`/`zM` and `foldmethod=indent`, with fold-aware movement and scrolling
- **Tags** — `Ctrl-]` / `Ctrl-T` and the `:tag` family over a `ctags -R .` table
- **TUI interface** — Dashboard, file browser, plugin manager, and floating overlays
- **Nerd Font file icons** — Per-filetype glyphs in the dashboard and file explorer, falling back to the lettered chip when no Nerd Font is installed

## Quick start

Run it from the source tree:

```sh
cargo run -p ctrlvim           # launch the editor
cargo run -p ctrlvim-core      # headless demo (no UI)
cargo test --workspace         # run all tests
```

Or install it. Building needs Rust 1.80+ and a C compiler (Lua 5.4 and
tree-sitter are vendored; on macOS that means `xcode-select --install`):

```sh
sudo make install                 # /usr/local/bin/cvi
make install PREFIX=~/.local      # no root; needs ~/.local/bin on PATH
make user-config                  # seed ~/.config/ctrlvim/config.toml
sudo make uninstall               # remove it again
```

Those targets work as-is on macOS, building for the host architecture. For one
binary that runs on both Apple Silicon and Intel:

```sh
make macos-deps                   # rustup target add, once
make macos                        # universal arm64 + x86_64, ad-hoc signed
sudo make macos-install           # ...and install it
```

`make help` lists every target. The binary is `cvi`:

```sh
cvi                  # dashboard for the current directory
cvi src/             # dashboard for a directory
cvi a.rs b.rs        # open files, cursor in the first
cvi --help           # usage
cvi --version
```

## Architecture

The project is organized into focused crates, each handling one concern:

| Crate | Purpose |
|-------|---------|
| `ctrlvim-text` | Rope-backed buffers, marks, undo tree, registers |
| `ctrlvim-editor` | Motions, operators, text objects, window splits |
| `ctrlvim-regex` | Vim regex engine: magic levels, backrefs, lookaround, `\zs`/`\ze` |
| `ctrlvim-lua` | Lua embedding and `vim.*` API compatibility |
| `ctrlvim-api` | `#[ctrlvim_api]` dispatch generation |
| `ctrlvim-treesitter` | Tree-sitter integration + the `highlights.scm` → styled-span highlighter |
| `ctrlvim-async` | Tokio event loop and msgpack-RPC |
| `ctrlvim-vimscript` | Vimscript interpreter |
| `ctrlvim-tui` | Terminal UI (Ratatui + crossterm) |

## License

Apache-2.0
