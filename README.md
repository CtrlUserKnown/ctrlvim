# ctrlvim / cvi

**ctrlvim** is a modern text editor built from scratch in Rust, designed as a faithful reimplementation of Neovim's editing model. **cvi** is its terminal UI, built with [Ratatui](https://github.com/ratatui/ratatui).

If you love Vim, but have ever wished for an editor that could be extended with Rust as easily as Lua, this is for you.

## Why?

Neovim is incredible — but its C codebase is a mountain of complexity. ctrlvim reimagines the core from the ground up in memory-safe Rust, preserving what makes Vim great (modal editing, extensibility, terminal-native feel) while making the internals approachable and hackable.

## Features

- **Modal editing that just works** — Normal, Insert, Visual, and Command-line modes with motions (`w`, `b`, `e`, `{`, `}`), operators (`d`, `y`, `c`), and text objects (`iw`, `a"`, `i(`). Counts compose the way Vim's do, so `2d3w` deletes six words
- **TOML configuration** — `~/.config/ctrlvim/config.toml` declares options, per-mode keymaps, autocommands, user commands, and plugins. No config script to write; see [`docs/config.example.toml`](docs/config.example.toml)
- **A real mapping layer** — `<C-x>`, `<A-x>`/`<M-x>`, `<C-S-x>`, arrows, F-keys and `<S-Tab>` are all bindable, in Normal, Insert *and* Visual mode. Overlapping chords resolve on `'timeoutlen'` the way Vim's do, so `<leader>q` and `<leader>qq` can coexist. `<leader>` is configurable, every built-in is listed by `:map` and removable with `[[unmap]]`, and a mapping that doesn't parse is reported instead of silently doing nothing
- **Discoverable keys** — every mapping carries a `desc`, settable from config, `:nnoremap <desc=…>`, or `vim.keymap.set(…, { desc = … })`. Hold a chord and a which-key popup lists what can follow it; `?` shows the whole table. Both read the live mapping table, so your own bindings appear with no code change and a stale row is impossible
- **Pinned files** — `:Pin` a handful of files and jump between them with `<A-1>`..`<A-5>`, `<A-n>`/`<A-p>`. Ordered by you and stable, unlike the buffer list, and remembered per project
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
- **Inline AI suggestions** — Copilot-style ghost text ahead of the cursor, from **CodeGemma-2B running locally** on [candle](https://github.com/huggingface/candle): no API key, no network at edit time, nothing leaving the machine. `<Tab>` accepts, `<C-l>`/`<C-j>` take a word or a line, `<C-e>` dismisses. Fill-in-the-middle, so the model sees the code on *both* sides of the cursor. Runs 4-bit quantized (1.6GB) by default, so it fits in ~2.8GB of RAM. Build it in with `--features ai`, then switch it on (Settings tab or `:AI on`); see [`docs/ai.md`](docs/ai.md)
- **TUI interface** — Dashboard, file browser, plugin manager, and floating overlays
- **Nerd Font file icons** — Per-filetype glyphs in the dashboard and file explorer, falling back to the lettered chip when no Nerd Font is installed

## Keybindings

`<leader>` is Space. Press `?` for the full list — it's generated from the live
mapping table, so it always matches what the keys actually do.

| Key | Does |
|-----|------|
| `<leader>e` / `<leader>ff` | fuzzy file browser |
| `<leader>w` / `<leader>q` / `<leader>qq` | write / write+quit / quit without saving |
| `<leader>S` | find & replace across the project |
| `<leader>d` / `<leader>p` | dashboard / command palette |
| `<leader>1`–`<leader>9` | go to tab N |
| `<leader>a` / `<leader>ar` / `<leader>ac` | pin this file / unpin it / unpin all |
| `<leader>h` | list pinned files |
| `<A-1>`–`<A-5>`, `<A-n>` / `<A-p>` | go to pinned file N / next / previous |
| `<A-j>` / `<A-k>` | move the line (or selection) down / up |
| `<C-S-j>` / `<C-S-k>` | duplicate the line (or selection) below / above |
| `<C-b>` / `<C-g>` | file drawer / markdown rendering |
| `jk` (insert) | leave Insert mode |

Every one of these is an ordinary mapping expanding to an Ex command, so any of
them can be replaced with `[[keymap]]`, removed with `[[unmap]]`, or turned off
wholesale with `[keymaps] defaults = false`.

Two caveats worth knowing. `<C-S-j>` is only distinguishable from `<C-j>` in a
terminal that supports the kitty keyboard protocol (kitty, WezTerm, foot,
Ghostty, recent Alacritty); elsewhere those two bindings simply never fire.
And bindings that would shadow a standard Vim motion — `H`/`L`, `[[`/`]]`, `;`
— are deliberately *not* defaults; they ship commented out in
[`docs/config.example.toml`](docs/config.example.toml) if you want them.

## Quick start

Run it from the source tree:

```sh
cargo run -p ctrlvim           # launch the editor
cargo run -p ctrlvim-core      # headless demo (no UI)
cargo test --workspace         # run all tests
```

Or install it. Building needs Rust 1.80+ and a C compiler (Lua 5.4 and
tree-sitter are vendored; on macOS that means `xcode-select --install`) —
there are no prebuilt release binaries yet, so every path below builds from
source:

```sh
curl -fsSL https://ctrluserknown.github.io/ctrlvim/install.sh | sh
```

Installs to `~/.local` — no root needed. It's a plain POSIX shell script that
clones the repo (or updates an existing clone) and runs `make install` for
you; read it before piping it to `sh` if you'd rather see exactly what it
does first. Or skip the installer and drive the Makefile yourself:

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
