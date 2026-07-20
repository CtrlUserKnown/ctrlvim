//! Keyboard handling.
//!
//! Two regimes share one entry point:
//! - **Editor focus** (a File buffer is active with no overlay open): keys are
//!   translated to [`ctrlvim_core::Key`] and fed to the engine's editing
//!   session, so motions/operators/insert all run in the real backend. A few
//!   Ctrl-chords (plus `:`/`?` in Normal mode) escape back to the shell so you
//!   are never trapped in the buffer.
//! - **Shell** (dashboard / plugin manager / any overlay): the design's
//!   navigation keymap, a direct port of the prototype's `handleKeyDown`.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use ctrlvim_core::Key;

use crate::app::{Action, App, DashboardSection, Layout, PanelId};

pub fn handle_key(app: &mut App, key: KeyEvent) {
    // Global quit, honored even mid-insert.
    if key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('q'))
    {
        app.should_quit = true;
        return;
    }

    // The command palette owns all input while open.
    if app.palette_open {
        handle_palette(app, key);
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

    // In Normal mode a handful of keys drive the shell instead of the buffer.
    // In Insert/Visual everything (including Tab and Ctrl-chords) goes to the
    // engine, with Esc returning to Normal.
    if normal {
        match key.code {
            KeyCode::Left if ctrl => return app.cycle_buffer(-1),
            KeyCode::Right if ctrl => return app.cycle_buffer(1),
            _ => {}
        }
        if ctrl && c == Some('b') {
            return app.dispatch(Action::ToggleSidebar);
        }
        if ctrl && c == Some('p') {
            return app.dispatch(Action::OpenPalette);
        }
        // Toggle live markdown rendering (no-op on non-markdown buffers).
        if ctrl && c == Some('g') {
            return app.dispatch(Action::ToggleMarkdown);
        }
        if !ctrl && c == Some(':') {
            return app.dispatch(Action::OpenPalette);
        }
        if !ctrl && c == Some('?') {
            return app.dispatch(Action::ToggleHelp);
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

// --- command palette -------------------------------------------------------

fn handle_palette(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Esc => app.dispatch(Action::ClosePalette),
        KeyCode::Down => app.move_palette(1),
        KeyCode::Up => app.move_palette(-1),
        KeyCode::Enter => {
            let idx = app.palette_index.min(app.palette_results().len().saturating_sub(1));
            app.dispatch(Action::RunPalette(idx));
        }
        KeyCode::Backspace => app.palette_backspace(),
        KeyCode::Char(ch) => app.palette_type(ch),
        _ => {}
    }
}

// --- shell (dashboard / plugin manager / overlays) -------------------------

fn handle_shell(app: &mut App, key: KeyEvent) {
    let c = char_of(&key);

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

    if c == Some(':') {
        app.dispatch(Action::OpenPalette);
        return;
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) && c == Some('b') {
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

    if c == Some('p') {
        app.open_plugins();
        return;
    }

    let workspace = on_dashboard && app.section == DashboardSection::Workspace;

    if workspace {
        match c {
            Some('r') => { app.dispatch(Action::TogglePanel(PanelId::RecentFiles)); return; }
            Some('g') => { app.dispatch(Action::TogglePanel(PanelId::Git)); return; }
            Some('b') => { app.dispatch(Action::TogglePanel(PanelId::Keybindings)); return; }
            _ => {}
        }
        if c == Some('2') { app.dispatch(Action::SetLayout(Layout::Columns)); return; }
        if c == Some('3') { app.dispatch(Action::SetLayout(Layout::Grid)); return; }
    }

    // Settings: j/k move the LSP list, Enter/Space toggle.
    if on_dashboard && app.section == DashboardSection::Settings {
        match (key.code, c) {
            (KeyCode::Down, _) | (_, Some('j')) => { app.move_lsp_selection(1); return; }
            (KeyCode::Up, _) | (_, Some('k')) => { app.move_lsp_selection(-1); return; }
            (KeyCode::Enter, _) | (KeyCode::Char(' '), _) => {
                app.toggle_lsp(app.lsp_index);
                return;
            }
            _ => {}
        }
    }

    // Explorer overlay or workspace file list: j/k move the file selection.
    if app.sidebar_visible || workspace {
        match (key.code, c) {
            (KeyCode::Down, _) | (_, Some('j')) => { app.move_file_selection(1); return; }
            (KeyCode::Up, _) | (_, Some('k')) => { app.move_file_selection(-1); return; }
            (KeyCode::Enter, _) => {
                // Open the selected file (from the explorer or the dashboard).
                app.open_file(app.file_index);
                return;
            }
            _ => {}
        }
    }

    // Escape closes the explorer if open.
    if key.code == KeyCode::Esc && app.sidebar_visible {
        app.dispatch(Action::CloseSidebar);
    }
}

/// Lowercased char for a `KeyCode::Char`, else `None`.
fn char_of(key: &KeyEvent) -> Option<char> {
    match key.code {
        KeyCode::Char(c) => Some(c.to_ascii_lowercase()),
        _ => None,
    }
}
