# ctrlvim-tui

The **Ratatui + crossterm frontend** for the ctrlvim engine — a startup
dashboard and editor shell that renders on top of `ctrlvim-core`.

It is a faithful reimplementation of the "Charvim Dashboard" design
(`design_handoff_charvim_dashboard/` in the design bundle) as a real terminal
UI: exact Tokyo Night palette, copy, and keybindings, driven by **both keyboard
and mouse**.

```sh
cargo run -p ctrlvim-tui            # launch the TUI (binary: `cvi`)
cargo test -p ctrlvim-tui           # render smoke tests (all screens + fuzz sizes)
cargo run -p ctrlvim-tui --example snapshot -- grid   # print a text snapshot of a screen
```

## Screens

- **Dashboard** with three sections — `workspace` (a `[2] columns` / `[3] grid`
  layout switcher over Recent Files / Sessions / Stats / Git Status / Plugins
  panels, the grid panels expandable), `settings` (LSP server table with
  on/off toggles), and `about` (version info).
- A persistent **Keybindings** panel to the left of the workspace section.
- **Plugin Manager** buffer, and a **live file editor**: opening a file loads
  it into the engine and the buffer is edited for real — motions, operators,
  and insert mode all run in `ctrlvim-core`, with the cursor and mode driven by
  the backend.
- **Tree-sitter syntax highlighting** for filetypes the engine ships a grammar
  for (Rust, JSON). `ctrlvim-core::syntax` returns per-line spans classed as
  keyword/type/string/…; `theme::syn_style` dresses them in the active theme, so
  highlighting follows a `:colorscheme` change. Only the visible rows are
  highlighted, cached until the buffer or viewport changes.
- Floating **file explorer** (`Ctrl+B`), **command palette** (`:`), and
  **help** (`?`) overlays, dismissable with `Esc` or a click outside.
- **Tags**: `Ctrl-]` jumps to the definition under the cursor, `Ctrl-T` returns,
  and `:tag`/`:tnext`/`:tprev`/`:tags` walk the matches. Generate the table with
  `ctags -R .`; a regenerated file is picked up automatically (the load checks
  its mtime), and pattern addresses still resolve after the definition moves.
- **Folds**: `zf{motion}` / `:{range}fold` to create, `za`/`zo`/`zc`, `zR`/`zM`,
  `zj`/`zk`, `zd`/`zE`, `zi`, and `:set foldmethod=indent`. A closed fold draws a
  `+--  9 lines: …` summary row; `j`/`k` and scrolling step over it.
- **Quickfix pane** along the bottom (`:copen`), filled by `:vimgrep /pat/ glob`
  (in-process) or `:make` / `:grep` (spawned, streaming into the list as they
  run). `:cnext`/`:cprev`/`:cc` jump; rows are clickable; `j`/`k`/`Enter` work
  in the pane when no file buffer has focus.
- **File icons** on every file row (recent files, drawer, explorer): a Nerd Font
  glyph per filetype, or the lettered chip when no Nerd Font is installed.
  `icons = "auto" | "nerd" | "text"` in `config.toml` (or the Settings row /
  `i`) overrides the detection; `CTRLVIM_NERD_FONT=1|0` forces it for `auto`.

## Keymap

**Shell** (dashboard / plugin manager / overlays): `:` palette · `Ctrl+B`
explorer · `Tab`/`Shift+Tab` cycle buffers · `[`/`]` cycle dashboard section ·
`w`/`s`/`a` jump to section · `p` plugin manager · `2`/`3` layout · `r`/`g`/`b`
expand panels · `j`/`k` move selection · `Enter` open/toggle · `?` help · `Esc`
close overlay. Every list row, tab, pill, and toggle is also clickable, and the
wheel scrolls the editor view (`mouse = false` gives it back to the terminal).

**Editor** (a file buffer is focused): keys go straight to the engine — Vim
motions/operators/insert all work. A few chords escape back to the shell:
`Ctrl+←`/`Ctrl+→` cycle buffers, `Ctrl+B` explorer, `Ctrl+P` or `:` palette,
`?` help (all in Normal mode). `Esc` returns to Normal. `Ctrl+C` quits from
anywhere.

## Architecture

| module | role |
|--------|------|
| `app` | `App` state + `Action` enum; the port of the design prototype's component, plus the owned `ctrlvim_core::Ctrlvim` engine |
| `input` | keyboard handling: editor-focus routes to the engine, shell keymap otherwise |
| `model` | domain types + the design's static mock data + file seed text |
| `icons` | Nerd Font detection + the per-filetype icon table's glyph/text modes |
| `theme` | Tokyo Night palette constants |
| `ui/*` | rendering: shell (tab bar/status line), dashboard, plugins, file editor, overlays |

**Backend connection.** `App` owns a real `ctrlvim_core::Ctrlvim`. A **File
buffer is a live editor window**: keystrokes are translated to
`ctrlvim_core::Key` and fed to `Session::feed`, and the view renders
`engine.lines()` with a block cursor at `engine.cursor()` and the mode from
`engine.mode()`. Because the facade exposes one working buffer, each file tab
keeps its own cached text: switching snapshots the outgoing file and loads the
incoming one (`App::set_active`), giving multi-buffer editing today and dropping
away cleanly once the engine holds multiple buffers itself.

The dashboard's recent-files / git / plugin / LSP data is still static mock data
until the engine grows sources for it — those live in `model` as plain structs
so they can be swapped for engine-fed data later.

Mouse support uses a per-frame **click-zone registry**: each interactive element
registers a `Rect` + `Action`; a click dispatches the same `Action` its
keyboard equivalent would, so keyboard and mouse never diverge. The wheel moves
`App::view_top`, a viewport offset in *screen rows* (so a closed fold counts
once); the renderer clamps it against the cursor, which is why keyboard movement
scrolls the view without anyone tracking the viewport, and why scrolling drags
the cursor only when it would otherwise leave the window.
