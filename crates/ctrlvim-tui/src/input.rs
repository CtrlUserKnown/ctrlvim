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

use ctrlvim_core::{Key, Mods, SpecialKey};

use crate::app::{Action, App, DashboardSection, PanelId};
use crate::replace::Field;

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
    if app.replace.is_some() {
        handle_replace(app, key);
        return;
    }
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
    if app.shell_open {
        handle_shell_output(app, key);
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
        // `:` opens the unified command palette (the command line's new UI)
        // rather than dropping the engine into its own Cmdline mode. This one
        // stays here: it's mode entry, not a mapping.
        if c == Some(':') {
            return app.dispatch(Action::OpenPalette);
        }
        // `Ctrl-B` (drawer), `Ctrl-G` (markdown) and the arrow motions used to
        // be hardcoded here too. They are default mappings now
        // (`session::DEFAULT_KEYMAPS`), so they reach the engine like any other
        // key — which is what makes them listable in `?` and rebindable.
    }

    if let Some(k) = to_engine_key(&key) {
        app.feed_engine(k);
    }
}

/// Translate a crossterm key into the engine's [`Key`], or `None` for keys the
/// engine has no representation for.
/// Translate a crossterm event into the engine's [`Key`].
///
/// Shift on a character is carried as the character's case, which is what the
/// terminal already reports for printable keys — so `<C-S-j>` arrives as
/// `Ctrl('J')` and `<C-j>` as `Ctrl('j')`. Most terminals can only tell those
/// apart when the keyboard-enhancement protocol is active (see
/// `main::enable_key_disambiguation`); without it both report as `<C-j>` and
/// the shifted mapping simply never fires.
fn to_engine_key(key: &KeyEvent) -> Option<Key> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let mods = Mods { ctrl, alt, shift };

    let special = |k: SpecialKey| Some(Key::Special { key: k, mods });
    match key.code {
        KeyCode::Char(c) => {
            // A shifted printable already arrives uppercased; fold it in
            // explicitly too, for terminals that report the base char instead.
            let c = if shift { c.to_ascii_uppercase() } else { c };
            match (ctrl, alt) {
                // Ctrl and Alt together has no portable encoding, and the
                // engine has no key for it — drop it rather than misreport it
                // as one or the other.
                (true, true) => None,
                (true, false) => Some(Key::Ctrl(c)),
                (false, true) => Some(Key::Alt(c)),
                (false, false) => Some(Key::Char(c)),
            }
        }
        KeyCode::Enter => Some(Key::Enter),
        KeyCode::Backspace => Some(Key::Backspace),
        KeyCode::Tab => Some(Key::Tab),
        KeyCode::Esc => Some(Key::Esc),
        KeyCode::BackTab => Some(Key::Special {
            key: SpecialKey::BackTab,
            // BackTab *is* the shifted Tab; keeping the flag as well would
            // make `<S-Tab>` unmatchable, since parsing normalizes it away.
            mods: Mods { shift: false, ..mods },
        }),
        KeyCode::Up => special(SpecialKey::Up),
        KeyCode::Down => special(SpecialKey::Down),
        KeyCode::Left => special(SpecialKey::Left),
        KeyCode::Right => special(SpecialKey::Right),
        KeyCode::Home => special(SpecialKey::Home),
        KeyCode::End => special(SpecialKey::End),
        KeyCode::PageUp => special(SpecialKey::PageUp),
        KeyCode::PageDown => special(SpecialKey::PageDown),
        KeyCode::Delete => special(SpecialKey::Delete),
        KeyCode::Insert => special(SpecialKey::Insert),
        KeyCode::F(n) if (1..=12).contains(&n) => special(SpecialKey::F(n)),
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
        _ if is_kill_to_start(&key) => app.finder_clear_to_start(),
        _ if is_word_backspace(&key) => app.finder_word_backspace(),
        KeyCode::Backspace => app.finder_backspace(),
        KeyCode::Char(c) => app.finder_type(c),
        _ => {}
    }
}

// --- find & replace panel --------------------------------------------------

/// Keys in the replace panel. The two text fields take typing; the results list
/// takes Vim-ish commands (`j`/`k`, `y`, `Y`, `<CR>`), which is why the same
/// letter means different things depending on focus.
fn handle_replace(app: &mut App, key: KeyEvent) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let results = app.replace.as_ref().is_some_and(|p| p.focus == Field::Results);
    // Grep-only mode (`OpenGrepPrompt`) has no Replace field, so accepting a
    // match must never rewrite anything — see `ReplacePanel::search_only`.
    let search_only = app.replace.as_ref().is_some_and(|p| p.search_only);

    // Chords that work from every field, so accepting doesn't require a detour
    // through the results list first.
    match key.code {
        KeyCode::Esc => return app.dispatch(Action::CloseReplace),
        KeyCode::Tab => return app.replace_cycle(),
        KeyCode::BackTab => return app.replace_cycle_back(),
        KeyCode::Down => return app.replace_move(1),
        KeyCode::Up => return app.replace_move(-1),
        KeyCode::Char('n') if ctrl => return app.replace_move(1),
        KeyCode::Char('p') if ctrl => return app.replace_move(-1),
        KeyCode::Char('i') if ctrl => return app.replace_toggle_case(),
        KeyCode::Char('a') if ctrl && !search_only => return app.replace_accept_all(),
        _ => {}
    }

    if results {
        match (key.code, char_of(&key)) {
            (KeyCode::Enter, _) => app.replace_jump(),
            (_, Some('j')) => app.replace_move(1),
            (_, Some('k')) => app.replace_move(-1),
            (KeyCode::Char('y'), _) if !search_only => app.replace_accept_one(),
            (KeyCode::Char('Y'), _) if !search_only => app.replace_accept_all(),
            (_, Some('q')) => app.dispatch(Action::CloseReplace),
            _ => {}
        }
        return;
    }

    // A text field: Enter jumps to the selected match in grep-only mode
    // (there is nothing to replace), or accepts every replacement otherwise.
    match key.code {
        KeyCode::Enter if search_only => app.replace_jump(),
        KeyCode::Enter => app.replace_accept_all(),
        _ if is_kill_to_start(&key) => app.replace_clear_to_start(),
        _ if is_word_backspace(&key) => app.replace_word_backspace(),
        KeyCode::Backspace => app.replace_backspace(),
        KeyCode::Char(c) => app.replace_type(c),
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
        _ if is_kill_to_start(&key) => app.palette_clear_to_start(),
        _ if is_word_backspace(&key) => app.palette_word_backspace(),
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
        _ if is_kill_to_start(&key) => app.save_prompt_clear_to_start(),
        _ if is_word_backspace(&key) => app.save_prompt_word_backspace(),
        KeyCode::Backspace => app.save_prompt_backspace(),
        KeyCode::Char(c) => app.save_prompt_type(c),
        _ => {}
    }
}

// --- `:!{cmd}` output overlay ------------------------------------------------

/// Keys while the shell-command output overlay is open: `j`/`k`/arrows scroll,
/// `Esc`/`Enter`/`q` dismiss.
fn handle_shell_output(app: &mut App, key: KeyEvent) {
    let c = char_of(&key);
    match key.code {
        KeyCode::Esc | KeyCode::Enter => app.dispatch(Action::CloseShellOutput),
        KeyCode::Down => app.scroll_shell_output(1),
        KeyCode::Up => app.scroll_shell_output(-1),
        _ if c == Some('j') => app.scroll_shell_output(1),
        _ if c == Some('k') => app.scroll_shell_output(-1),
        _ if c == Some('q') => app.dispatch(Action::CloseShellOutput),
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

    // Mappings, from the same table the editor uses. This used to be a
    // hand-rolled leader machine that knew only `<leader>1-9`, `<leader>d` and
    // `<leader>S`, so a user's `[[keymap]]` entries did nothing on the
    // dashboard. A chord that isn't a mapping falls through to the shell's own
    // navigation keys below.
    if let Some(k) = to_engine_key(&key) {
        if app.shell_keymap(k) {
            return;
        }
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

    // The quickfix pane owns j/k/Enter while it's open and no file buffer has
    // focus. From inside a file buffer the keys belong to the editor, so `:cn`
    // / `:cp` are the way to walk the list there — as in Vim.
    if app.quickfix_open && !app.editor_focus() {
        match (key.code, c) {
            (KeyCode::Down, _) | (_, Some('j')) => { app.move_quickfix_selection(1); return; }
            (KeyCode::Up, _) | (_, Some('k')) => { app.move_quickfix_selection(-1); return; }
            (KeyCode::Enter, _) => { app.quickfix_select(app.quickfix_index); return; }
            _ => {}
        }
    }

    let on_dashboard = app.is_dashboard();

    if on_dashboard && (c == Some('[') || c == Some(']')) {
        app.cycle_section(if c == Some(']') { 1 } else { -1 });
        return;
    }
    if on_dashboard {
        match c {
            Some('1') => { app.section = DashboardSection::Workspace; return; }
            Some('2') => { app.section = DashboardSection::Settings; return; }
            Some('3') => { app.section = DashboardSection::About; return; }
            _ => {}
        }
    }

    if !ctrl && c == Some('p') {
        app.open_plugins();
        return;
    }

    let workspace = on_dashboard && app.section == DashboardSection::Workspace;

    // Workspace: `g` expands the git panel; `e`/`f` open the fuzzy browser.
    // `c`/`l`/`d`/`F` are the git pane's own actions, all read-only — see the
    // legend drawn at the foot of the panel.
    if workspace {
        if c == Some('g') { app.dispatch(Action::TogglePanel(PanelId::Git)); return; }
        if c == Some('e') || c == Some('f') { app.dispatch(Action::OpenFinder); return; }
        if c == Some('c') { app.dispatch(Action::GitChangedFiles); return; }
        if c == Some('l') { app.dispatch(Action::GitLog); return; }
        if c == Some('d') { app.dispatch(Action::GitDiff); return; }
        if c == Some('F') { app.dispatch(Action::GitFetch); return; }
        if c == Some('X') { app.dispatch(Action::DiscardSession); return; }
        // ACTIONS panel rows with no existing binding elsewhere on the
        // dashboard. `:`/`?` (Command Palette / Help) already work globally —
        // their rows are discoverability only, same as `n`/`e` above.
        if c == Some('/') { app.dispatch(Action::OpenGrepPrompt); return; }
        if c == Some('q') { app.dispatch(Action::RunEx("qa".to_string())); return; }
    }

    // Settings: j/k scroll continuously through the EDITOR options and the
    // tools list; Enter/Space toggles the focused row. `d`/`m` jump-toggle
    // directly, `I` installs the focused tool (rust_analyzer, stylua, …).
    if on_dashboard && app.section == DashboardSection::Settings {
        match (key.code, c) {
            (KeyCode::Char('I'), _) => { app.install_focused_tool(); return; }
            (_, Some('d')) => { app.dispatch(Action::ToggleStartupDrawer); return; }
            (_, Some('m')) => { app.dispatch(Action::ToggleMouse); return; }
            (_, Some('i')) => { app.dispatch(Action::CycleIconMode); return; }
            (_, Some('a')) => { app.dispatch(Action::ToggleAi); return; }
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
            KeyCode::Down => app.drawer_move(1),
            KeyCode::Up => app.drawer_move(-1),
            KeyCode::Char('n') if ctrl => app.drawer_move(1),
            KeyCode::Char('p') if ctrl => app.drawer_move(-1),
            _ if is_kill_to_start(&key) => app.drawer_clear_to_start(),
            _ if is_word_backspace(&key) => app.drawer_word_backspace(),
            KeyCode::Backspace => app.drawer_backspace(),
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

// --- OS-level word/line deletion in single-line fields ----------------------
//
// The finder, replace, palette, save-as, and drawer-search fields are plain
// `String`s edited only at the end, so "delete previous word" and "delete to
// start of line" are just different truncations — but recognizing the key
// chords takes care, because none of these arrive as a distinct "modifier +
// Backspace" the way you'd expect:
//
// - macOS Option+Backspace (delete last word) decodes as `Backspace` with the
//   Alt modifier: the terminal sends ESC + DEL, and crossterm folds a leading
//   ESC into the Alt modifier of whatever follows.
// - Ctrl+Backspace (delete last word, the Linux convention) shares its
//   control byte (0x08) with Ctrl+H, so it decodes as `Char('h')` with
//   Ctrl — there is no way to tell it apart from someone actually typing
//   Ctrl+H, but nobody does that on purpose in a text field.
// - macOS Cmd+Backspace (delete to start of line) never reaches the terminal
//   as "Cmd + anything" — Cmd chords are consumed by the terminal emulator
//   itself. Terminals that bind this at all (iTerm2/WezTerm/Ghostty's
//   "natural text editing" presets) forward it as Ctrl+U, readline's
//   `unix-line-discard` byte.
//
// Without this, all of the above used to fall through to the fields' generic
// `KeyCode::Char(c) => type(c)` arm and insert a literal 'h' or 'u'.

/// "Delete the previous word": Option+Backspace, Ctrl+Backspace, or Ctrl+W
/// (a common terminal remap for the same thing).
fn is_word_backspace(key: &KeyEvent) -> bool {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    match key.code {
        KeyCode::Backspace => ctrl || alt,
        KeyCode::Char('h') | KeyCode::Char('w') => ctrl,
        _ => false,
    }
}

/// "Delete to the start of the line": Cmd+Backspace (forwarded as Ctrl+U by
/// terminals that bind it) or a literal Super+Backspace, for terminals with
/// the kitty keyboard protocol enabled that report Cmd directly.
fn is_kill_to_start(key: &KeyEvent) -> bool {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let super_ = key.modifiers.contains(KeyModifiers::SUPER);
    match key.code {
        KeyCode::Char('u') => ctrl,
        KeyCode::Backspace => super_,
        _ => false,
    }
}

#[cfg(test)]
mod os_keyboard_tests {
    use super::*;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn option_backspace_on_macos_is_a_word_backspace() {
        // The terminal sends ESC + DEL; crossterm folds that into Alt+Backspace.
        assert!(is_word_backspace(&key(KeyCode::Backspace, KeyModifiers::ALT)));
    }

    #[test]
    fn ctrl_backspace_on_linux_is_a_word_backspace() {
        // Ctrl+Backspace shares its control byte with Ctrl+H.
        assert!(is_word_backspace(&key(KeyCode::Char('h'), KeyModifiers::CONTROL)));
        assert!(is_word_backspace(&key(KeyCode::Backspace, KeyModifiers::CONTROL)));
    }

    #[test]
    fn ctrl_w_is_also_a_word_backspace() {
        assert!(is_word_backspace(&key(KeyCode::Char('w'), KeyModifiers::CONTROL)));
    }

    #[test]
    fn cmd_backspace_on_macos_kills_to_start() {
        // Forwarded by the terminal as Ctrl+U (readline unix-line-discard).
        assert!(is_kill_to_start(&key(KeyCode::Char('u'), KeyModifiers::CONTROL)));
        assert!(is_kill_to_start(&key(KeyCode::Backspace, KeyModifiers::SUPER)));
    }

    #[test]
    fn plain_backspace_and_typing_are_neither() {
        assert!(!is_word_backspace(&key(KeyCode::Backspace, KeyModifiers::NONE)));
        assert!(!is_kill_to_start(&key(KeyCode::Backspace, KeyModifiers::NONE)));
        assert!(!is_word_backspace(&key(KeyCode::Char('h'), KeyModifiers::NONE)));
        assert!(!is_kill_to_start(&key(KeyCode::Char('u'), KeyModifiers::NONE)));
    }
}
