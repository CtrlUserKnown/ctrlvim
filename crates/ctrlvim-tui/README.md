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
- Floating **file explorer** (`Ctrl+B`), **command palette** (`:`), and
  **help** (`?`) overlays, dismissable with `Esc` or a click outside.

## Keymap

**Shell** (dashboard / plugin manager / overlays): `:` palette · `Ctrl+B`
explorer · `Tab`/`Shift+Tab` cycle buffers · `[`/`]` cycle dashboard section ·
`w`/`s`/`a` jump to section · `p` plugin manager · `2`/`3` layout · `r`/`g`/`b`
expand panels · `j`/`k` move selection · `Enter` open/toggle · `?` help · `Esc`
close overlay. Every list row, tab, pill, and toggle is also clickable.

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
keyboard equivalent would, so keyboard and mouse never diverge.
