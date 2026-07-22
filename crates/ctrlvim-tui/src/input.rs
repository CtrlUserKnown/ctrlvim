//! Keyboard handling.
//!
//! Input is routed to one of a few consumers, in priority order:
//! - **Engine command line** (`:`): once the engine is in Cmdline mode it owns
//!   all keys until `<CR>`/`<Esc>`, from any screen — so `:q` works on the
//!   dashboard too. Ex commands run in the engine and emit host effects.
//! - **Finder / palette overlays**: modal frontend widgets.
//! - **Editor focus** (a File buffer, no overlay): keys become
//!   [`ctrlvim_core::Key`]s fed to the engine, so motions/operators/insert and
//!   `<leader>` mappings all run in the real backend. A few Ctrl-chords escape
//!   to frontend-only concerns (drawer, palette, markdown, buffer cycling).
//! - **Shell**: the dashboard / plugin manager navigation keymap.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use ctrlvim_core::Key;

use crate::app::{Action, App, DashboardSection, PanelId};

pub fn handle_key(app: &mut App, key: KeyEvent) {
    // Emergency quit, honored even mid-insert. (Ctrl-Q is intentionally not a
    // quit binding — quitting goes through `:q`/`:wq`.)
    if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c')) {
        app.should_quit = true;
        return;
    }

    // The engine's `:` command line owns all input while open, from any screen.
    if app.engine.cmdline().is_some() {
        if let Some(k) = to_engine_key(&key) {
            app.feed_engine(k);
        }
        return;
    }

    // Modal frontend overlays.
    if app.finder.is_some() {
        handle_finder(app, key);
        return;
    }
    if app.palette_open {
        handle_palette(app, key);
        return;
    }
    if app.save_prompt.is_some() {
        handle_save_prompt(app, key);
        return;
    }

    // Ctrl+Tab / Ctrl+Shift+Tab cycle through open tabs from anywhere (editor
    // or dashboard), like a browser.
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    if ctrl && matches!(key.code, KeyCode::Tab | KeyCode::BackTab) {
        let back = key.code == KeyCode::BackTab || key.modifiers.contains(KeyModifiers::SHIFT);
        app.cycle_buffer(if back { -1 } else { 1 });
        return;
    }

    // A live editor window takes keystrokes straight to the engine.
    if app.editor_focus() {
        handle_editor(app, key);
        return;
    }

    handle_shell(app, key);
}

// --- editor focus ----------------------------------------------------------

fn handle_editor(app: &mut App, key: KeyEvent) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let c = char_of(&key);
    let normal = app.editor_mode() == "n";

    // In Normal mode a few chords drive frontend-only concerns rather than the
    // buffer: the Ctrl-chords below, plus `:` which opens the unified command
    // palette. Everything else — `<Space>` (leader), Insert/Visual keys — goes
    // to the engine.
    if normal {
        match key.code {
            KeyCode::Left if ctrl => return app.cycle_buffer(-1),
            KeyCode::Right if ctrl => return app.cycle_buffer(1),
            _ => {}
        }
        if ctrl && c == Some('b') {
            return app.dispatch(Action::ToggleSidebar);
        }
        // `:` opens the unified command palette (the command line's new UI)
        // rather than dropping the engine into its own Cmdline mode.
        if c == Some(':') {
            return app.dispatch(Action::OpenPalette);
        }
        // Toggle live markdown rendering (no-op on non-markdown buffers).
        if ctrl && c == Some('g') {
            return app.dispatch(Action::ToggleMarkdown);
        }
        // Arrow keys act as hjkl motions (the engine's Key has no arrows).
        match key.code {
            KeyCode::Left => return app.feed_engine(Key::Char('h')),
            KeyCode::Down => return app.feed_engine(Key::Char('j')),
            KeyCode::Up => return app.feed_engine(Key::Char('k')),
            KeyCode::Right => return app.feed_engine(Key::Char('l')),
            _ => {}
        }
    }

    if let Some(k) = to_engine_key(&key) {
        app.feed_engine(k);
    }
}

/// Translate a crossterm key into the engine's [`Key`], or `None` for keys the
/// engine has no representation for.
fn to_engine_key(key: &KeyEvent) -> Option<Key> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Char(c) if ctrl => Some(Key::Ctrl(c.to_ascii_lowercase())),
        KeyCode::Char(c) => Some(Key::Char(c)),
        KeyCode::Enter => Some(Key::Enter),
        KeyCode::Backspace => Some(Key::Backspace),
        KeyCode::Tab => Some(Key::Tab),
        KeyCode::Esc => Some(Key::Esc),
        _ => None,
    }
}

// --- fuzzy file browser ----------------------------------------------------

fn handle_finder(app: &mut App, key: KeyEvent) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Esc => app.dispatch(Action::CloseFinder),
        KeyCode::Enter => app.finder_select(),
        KeyCode::Down => app.finder_move(1),
        KeyCode::Up => app.finder_move(-1),
        KeyCode::Char('n') if ctrl => app.finder_move(1),
        KeyCode::Char('p') if ctrl => app.finder_move(-1),
        KeyCode::Backspace => app.finder_backspace(),
        KeyCode::Char(c) => app.finder_type(c),
        _ => {}
    }
}

// --- command palette -------------------------------------------------------

fn handle_palette(app: &mut App, key: KeyEvent) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Esc => app.dispatch(Action::ClosePalette),
        KeyCode::Down => app.move_palette(1),
        KeyCode::Up => app.move_palette(-1),
        KeyCode::Char('n') if ctrl => app.move_palette(1),
        KeyCode::Char('p') if ctrl => app.move_palette(-1),
        KeyCode::Enter => app.submit_palette(),
        KeyCode::Backspace => app.palette_backspace(),
        KeyCode::Char(ch) => app.palette_type(ch),
        _ => {}
    }
}

// --- save-as prompt --------------------------------------------------------

fn handle_save_prompt(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Esc => app.close_save_prompt(),
        KeyCode::Enter => app.save_prompt_confirm(),
        KeyCode::Backspace => app.save_prompt_backspace(),
        KeyCode::Char(c) => app.save_prompt_type(c),
        _ => {}
    }
}

// --- shell (dashboard / plugin manager / drawer) ---------------------------

fn handle_shell(app: &mut App, key: KeyEvent) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let c = char_of(&key);

    // The file drawer, when open, handles its own keys (incl. `/` search).
    if app.sidebar_visible {
        handle_drawer(app, key);
        return;
    }

    // `<leader>{1-9}` jumps to that tab (the editor does this via the engine
    // keymap; the shell needs its own two-key handling).
    if app.leader_pending {
        app.leader_pending = false;
        if let Some(d) = c.and_then(|c| c.to_digit(10)) {
            if (1..=9).contains(&d) {
                app.set_active(d as usize - 1);
                return;
            }
        }
        if c == Some('d') {
            app.dispatch(Action::OpenDashboard);
            return;
        }
        // Not a recognized leader chord — fall through and handle normally.
    }
    if c == Some(' ') {
        app.leader_pending = true;
        return;
    }

    // '?' toggles help from anywhere in the shell.
    if c == Some('?') {
        app.dispatch(Action::ToggleHelp);
        return;
    }
    if app.help_open {
        if key.code == KeyCode::Esc {
            app.dispatch(Action::CloseHelp);
        }
        return;
    }

    // `:` opens the unified command palette (so `:q` etc. work off a file too).
    if c == Some(':') {
        app.dispatch(Action::OpenPalette);
        return;
    }
    // `n` starts a new file (opens the browser where a typed name is created).
    if c == Some('n') {
        app.dispatch(Action::NewFile);
        return;
    }
    if ctrl && c == Some('b') {
        app.dispatch(Action::ToggleSidebar);
        return;
    }
    if key.code == KeyCode::Tab {
        app.cycle_buffer(1);
        return;
    }
    if key.code == KeyCode::BackTab {
        app.cycle_buffer(-1);
        return;
    }

    let on_dashboard = app.is_dashboard();

    if on_dashboard && (c == Some('[') || c == Some(']')) {
        app.cycle_section(if c == Some(']') { 1 } else { -1 });
        return;
    }
    if on_dashboard {
        match c {
            Some('w') => { app.section = DashboardSection::Workspace; return; }
            Some('s') => { app.section = DashboardSection::Settings; return; }
            Some('a') => { app.section = DashboardSection::About; return; }
            _ => {}
        }
    }

    if !ctrl && c == Some('p') {
        app.open_plugins();
        return;
    }

    let workspace = on_dashboard && app.section == DashboardSection::Workspace;

    // Workspace: `g` expands the git panel; `e`/`f` open the fuzzy browser.
    if workspace {
        if c == Some('g') { app.dispatch(Action::TogglePanel(PanelId::Git)); return; }
        if c == Some('e') || c == Some('f') { app.dispatch(Action::OpenFinder); return; }
    }

    // Settings: j/k scroll continuously through the EDITOR options and the LSP
    // list; Enter/Space toggles the focused row. `d`/`m` jump-toggle directly.
    if on_dashboard && app.section == DashboardSection::Settings {
        match (key.code, c) {
            (_, Some('d')) => { app.dispatch(Action::ToggleStartupDrawer); return; }
            (_, Some('m')) => { app.dispatch(Action::ToggleMouse); return; }
            (KeyCode::Down, _) | (_, Some('j')) => { app.move_settings(1); return; }
            (KeyCode::Up, _) | (_, Some('k')) => { app.move_settings(-1); return; }
            (KeyCode::Enter, _) | (KeyCode::Char(' '), _) => {
                app.settings_toggle();
                return;
            }
            _ => {}
        }
    }

    // Workspace file list: j/k move the selection, Enter opens.
    if workspace {
        match (key.code, c) {
            (KeyCode::Down, _) | (_, Some('j')) => { app.move_file_selection(1); return; }
            (KeyCode::Up, _) | (_, Some('k')) => { app.move_file_selection(-1); return; }
            (KeyCode::Enter, _) => { app.open_file(app.file_index); return; }
            _ => {}
        }
    }
}

/// Keys while the file drawer (opt-in sidebar) is open, including `/` search.
fn handle_drawer(app: &mut App, key: KeyEvent) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    if app.drawer_search {
        match key.code {
            KeyCode::Esc => app.drawer_search = false, // back to navigation
            KeyCode::Enter => app.open_file(app.file_index),
            KeyCode::Backspace => app.drawer_backspace(),
            KeyCode::Down => app.drawer_move(1),
            KeyCode::Up => app.drawer_move(-1),
            KeyCode::Char('n') if ctrl => app.drawer_move(1),
            KeyCode::Char('p') if ctrl => app.drawer_move(-1),
            KeyCode::Char(c) => app.drawer_type(c),
            _ => {}
        }
        return;
    }
    match (key.code, char_of(&key)) {
        (KeyCode::Esc, _) => app.dispatch(Action::CloseSidebar),
        (KeyCode::Char('/'), _) => app.drawer_start_search(),
        (KeyCode::Down, _) | (_, Some('j')) => app.drawer_move(1),
        (KeyCode::Up, _) | (_, Some('k')) => app.drawer_move(-1),
        (KeyCode::Enter, _) => app.open_file(app.file_index),
        _ => {}
    }
}

/// Lowercased char for a `KeyCode::Char`, else `None`.
fn char_of(key: &KeyEvent) -> Option<char> {
    match key.code {
        KeyCode::Char(c) => Some(c.to_ascii_lowercase()),
        _ => None,
    }
}
