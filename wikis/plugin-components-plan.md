# User plugin components & layouts — plan

Status: **idea / not started**. This is a concept doc, not a build order.

## Motivation

ctrlvim's own philosophy is to bake the handful of things people actually
install plugins for (LSP, linting, formatting — see
[`lsp-mason-plan.md`](./lsp-mason-plan.md)) straight into the editor, so most
users never reach for a plugin manager. But some people will still want to
build things ctrlvim doesn't ship — a fuzzy finder, a git UI, a note-taking
sidebar. Today that means writing raw `vim.api.*` Lua against low-level
primitives (see `crates/ctrlvim-lua/src/host.rs`), the same way it works in
real Neovim. That's fine for people who already know that API, but it means
every plugin author re-invents floating windows, lists, and layout math from
scratch.

The idea: give plugin authors a small library of pre-built **components**
(list picker, input box, table, floating panel) and a **layout** system to
arrange them, so writing a plugin feels like composing widgets instead of
hand-rolling a TUI.

## Sketch of the pieces

### 1. Component library (`vim.components.*` or similar Lua namespace)

A handful of primitives, each backed by a Rust-side Ratatui widget exposed
through `mlua`, roughly:

- `Float` — a floating window (position, size, border, title) — the
  container everything else usually lives in.
- `List` / `Picker` — a scrollable, filterable list with a selection
  callback; the generic building block behind fuzzy-finder-style plugins.
- `Input` — a single-line prompt with a submit/cancel callback.
- `Table` — rows/columns, for things like a diagnostics list or git status.
- `StatusSegment` — a piece of text (with highlight groups) plugins can
  contribute to the status line.

Each component would be a Lua object with a small set of methods
(`:mount()`, `:unmount()`, `:on_select(fn)`, etc.), not a full widget
framework — closer to what `nui.nvim` provides for real Neovim than to a UI
framework like React.

### 2. Layout system

ctrlvim's own TUI already lays itself out with Ratatui's
`Layout`/`Direction`/`Constraint` (see `crates/ctrlvim-tui/src/ui/mod.rs`).
The plan is to expose that same engine to Lua as a small declarative API —
rows/columns of components with `Length`/`Percentage`/`Min` sizing — instead
of making plugin authors compute rectangles by hand. A plugin would describe
its UI as a tree of splits and components; ctrlvim would resolve that against
the terminal size the same way the built-in dashboard/file-browser panels do
today.

### 3. Wiring to editor state

Components need to react to editor events — cursor moves, buffer changes,
LSP diagnostics arriving, etc. ctrlvim-lua already has an autocmd system
(`ctrlvim_create_autocmd`/`fire_autocmd` in `host.rs`); components would
subscribe through that same mechanism rather than a new event system.

### 4. Packaging & distribution

No new package manager. Plugins keep using the existing pack-directory
convention ctrlvim already scans (`~/.config/ctrlvim/pack/*/{start,opt}/*`,
see `load_plugins()` in `crates/ctrlvim-tui/src/data.rs`) or the `plugin =
"path"` startup-script config already added in `config.toml`. "Pull from
components" means a documented, stable Lua API surface to build *against* —
not a registry of prebuilt plugins to download. (A registry of installable
*tools* — language servers, linters, formatters — is a separate, narrower
concern; see the Mason-equivalent plan.)

### 5. Proof of concept

The existing "Plugin Manager" screen (`crates/ctrlvim-tui/src/ui/plugins.rs`)
is currently a hand-built Rust panel with mock data. Once components +
layout exist, rebuilding that same screen as a Lua plugin using them would
be a good dogfood test of whether the API is actually expressive enough —
and would let the panel grow real data sources without more bespoke Rust UI
code.

## Rough phasing (not scheduled)

1. Design the component trait boundary in Rust and how it's bound into
   `mlua` (mounting, callbacks, teardown).
2. Implement `Float`, `List`/`Picker`, and `Input` — the three primitives
   almost every plugin idea needs.
3. Expose the Ratatui layout engine to Lua as a declarative split/constraint
   API.
4. Add `Table` and `StatusSegment` once a couple of real plugins want them.
5. Write one example plugin end-to-end (candidate: a fuzzy file picker,
   since it exercises `List` + `Float` + filtering) and document the API
   from that example.

## Explicitly out of scope for now

- A plugin registry / marketplace.
- Compatibility shims for existing Neovim plugin ecosystems (telescope.nvim,
  nui.nvim, etc.) — those depend on Neovim internals ctrlvim doesn't and
  won't replicate 1:1.
- Anything beyond the component/layout primitives themselves — this doc is
  about giving plugin authors better building blocks, not about deciding
  which plugins ctrlvim should ship with (it should ship with none).
