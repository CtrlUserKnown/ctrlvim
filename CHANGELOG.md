# Changelog

Newest first. There are no tagged releases yet — everything below is on
`main`, which is what the installer builds.

---

## Unreleased — v0.1.0

### 2026-08-02

- an LSP client — a declared server is spawned for the buffers it claims,
  kept in sync as you edit, and asked for completions
- `lsp.lua` — every language server and build linker is declared there and
  nowhere else; nothing is baked into the binary, so a server you don't name
  never appears anywhere in the editor
- a completion popup — server results refiltered locally as you type, with
  buffer words appended after them
- `<C-o>`/`<C-i>` carry on across files once a file's own jumplist is
  exhausted
- `cvi tool fetch-release` — a generic GitHub-release installer, for the
  servers with no `cargo`/`npm`/`pip` install path
- auto-closing brackets and quotes in Insert mode
- Settings: searchable rows, an indent-width cycle, a tab-bar toggle; a fresh
  install now writes `config.toml` on first launch
- the site rebuilt — the docs are generated from the wiki, so they're the same
  text the repository ships

### 2026-07-31

- `:Lint` runs a filetype's linter and fills the quickfix list
- `'guicursor'` — cursor shape per mode, with the idiomatic later-entry-wins
  parsing
- `'cursorline'`
- vendored Lua runtime, so a build needs no system Lua
- more tree-sitter grammars

### 2026-07-29

- line wrapping and horizontal scrolling, with click-to-cursor
- session persistence — the open-buffer list and a snapshot of unsaved text
  come back on the next launch in that project
- grep-only mode for the find panel
- dashboard overhaul: workspace, settings and about sections
- `install.sh` and the site
- inline AI suggestions — CodeGemma-2B on candle, ghost text,
  fill-in-the-middle, off unless built and switched on
- pinned files: `:Pin`, `<A-1>`..`<A-5>`, per project

### 2026-07-27

- the Vim regex engine — backreferences, lookaround, `\zs`/`\ze`, all four
  magic levels, non-greedy repeats, search offsets
- multi-session management
- msgpack-RPC server over a Unix socket
- the TUI dashboard
- `ctrlvim-tools`: the tool registry, detection and installation
- plugin commands and shell support
- Lua API expansion

### 2026-07-26

- project-wide find & replace (`:Find`, `<leader>S`) with a per-line
  before/after diff and `y`/`Y` to accept
- the mouse wheel scrolls the viewport independently of the cursor

### 2026-07-25

- tree-sitter syntax highlighting, themed from the active colorscheme
- folds (`zf`, `za`, `zR`, `zM`, `foldmethod=indent`)
- quickfix (`:make`, `:grep`, `:vimgrep`, `:copen`, `:cnext`)
- tags (`<C-]>`, `<C-t>`, the `:tag` family)
- the job system — a spawned program's output streams in without blocking the
  editor

### 2026-07-22

- `gt`/`gT` tab switching and the finder commands
- `:dash`

### 2026-07-21

- operators, Visual mode (char / line / block) and text objects

### 2026-07-20

- `ctrlvim-tui` — the Ratatui frontend
- `ctrlvim-markdown` — UI-less markdown analysis, for live rendering (`<C-g>`)

### 2026-07-10

- first commit: the engine crates — types, text, options, editor, api, lua,
  async, core

---

## Next

- the rest of LSP — diagnostics, go-to-definition, hover, rename; the client
  only does document sync and completion today
- incremental tree-sitter parsing; a full reparse still happens on every edit
- prebuilt binaries and a tagged release

The commit log is the authority:
<https://github.com/CtrlUserKnown/ctrlvim/commits/main>
