//! Render-level smoke tests: drive the app through every screen/overlay and a
//! range of terminal sizes, asserting content appears and nothing panics.
//!
//! Panels backed by real project data (recent files, git, plugins, LSP, stats)
//! are exercised against a controlled temp directory so assertions are
//! deterministic; structural checks (headers, labels) cover the rest.

use std::fs;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use ctrlvim_tui::app::{Action, App, DashboardSection, Layout, PanelId};
use ctrlvim_tui::{input, ui};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::TestBackend;
use ratatui::Terminal;

// --- helpers ---------------------------------------------------------------

static COUNTER: AtomicUsize = AtomicUsize::new(0);

/// Create a throwaway project directory with the given files (written in order,
/// so the last file is the most recently modified) and an app rooted at it.
fn temp_project(files: &[(&str, &str)]) -> App {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("ctrlvim-test-{}-{}", std::process::id(), n));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    for (name, content) in files {
        fs::write(dir.join(name), content).unwrap();
    }
    App::with_root(dir, Instant::now())
}

fn render(app: &App, w: u16, h: u16) -> String {
    let backend = TestBackend::new(w, h);
    let mut term = Terminal::new(backend).unwrap();
    term.draw(|f| {
        ui::draw(f, app);
    })
    .unwrap();
    let buf = term.backend().buffer().clone();
    let mut out = String::new();
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            out.push_str(buf[(x, y)].symbol());
        }
        out.push('\n');
    }
    out
}

fn contains_all(hay: &str, needles: &[&str]) {
    for n in needles {
        assert!(hay.contains(n), "expected to find {n:?} in rendered output:\n{hay}");
    }
}

fn key(app: &mut App, c: char) {
    input::handle_key(app, KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
}
fn press(app: &mut App, code: KeyCode) {
    input::handle_key(app, KeyEvent::new(code, KeyModifiers::NONE));
}
fn ctrl(app: &mut App, c: char) {
    input::handle_key(app, KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL));
}
fn typ(app: &mut App, s: &str) {
    for c in s.chars() {
        key(app, c);
    }
}

// --- dashboard structure ---------------------------------------------------

#[test]
fn dashboard_grid_default() {
    let app = temp_project(&[("main.rs", "fn main() {}\n")]);
    let out = render(&app, 130, 44);
    contains_all(
        &out,
        &[
            "charvim",
            "workspace",
            "settings",
            "about",
            "DASHBOARD LAYOUT",
            "grid",
            "RECENT FILES",
            "GIT STATUS",
            "PLUGINS",
            "KEYBINDINGS", // persistent sidebar
            "main.rs",     // real file from the temp project
            "NORMAL",
            "CHARVIM · TUI",
        ],
    );
}

#[test]
fn dashboard_columns_layout() {
    let mut app = temp_project(&[("main.rs", "fn main() {}\n")]);
    app.dispatch(Action::SetLayout(Layout::Columns));
    let out = render(&app, 130, 44);
    contains_all(&out, &["RECENT FILES", "SESSIONS", "STATS", "startup", "loc"]);
}

#[test]
fn recent_files_reflect_the_real_directory() {
    let app = temp_project(&[("alpha.rs", "a\n"), ("zeta.md", "z\n")]);
    let out = render(&app, 130, 44);
    // Both real files show up in the Recent Files panel.
    contains_all(&out, &["alpha.rs", "zeta.md"]);
}

#[test]
fn git_panel_shows_untracked_state() {
    // A temp dir is not a git repo, so the panel reports that honestly.
    let app = temp_project(&[("main.rs", "fn main() {}\n")]);
    assert!(app.project.git.is_none());
    let out = render(&app, 130, 44);
    assert!(out.contains("not a git repository"));
}

#[test]
fn empty_project_renders_empty_states() {
    let app = temp_project(&[]);
    let out = render(&app, 130, 44);
    contains_all(&out, &["no files"]);
}

// --- settings / about ------------------------------------------------------

#[test]
fn settings_tab_lists_servers() {
    let mut app = temp_project(&[("main.rs", "fn main() {}\n")]);
    app.dispatch(Action::GotoSection(DashboardSection::Settings));
    let out = render(&app, 130, 44);
    // Known servers are always listed; install/enable state varies by machine.
    contains_all(&out, &["LANGUAGE SERVERS", "rust-analyzer", "tsserver", "lsp.toml"]);
    assert!(!out.contains("┤ KEYBINDINGS ├")); // sidebar hidden outside workspace
}

#[test]
fn about_tab() {
    let mut app = temp_project(&[("main.rs", "fn main() {}\n")]);
    app.dispatch(Action::GotoSection(DashboardSection::About));
    let out = render(&app, 130, 44);
    contains_all(&out, &["charvim", "a rust tui editor", "ratatui", "0.29", "crossterm", "MIT"]);
}

// --- panels / plugin manager ----------------------------------------------

#[test]
fn grid_expand_git_reveals_more_when_repo() {
    // Uses the crate's own directory (inside a git repo) so git data exists.
    let root = std::env::current_dir().unwrap();
    let mut app = App::with_root(root, Instant::now());
    if app.project.git.is_none() {
        return; // not run inside a repo; nothing to assert
    }
    let collapsed = render(&app, 130, 44);
    assert!(!collapsed.contains("untracked"));
    app.dispatch(Action::TogglePanel(PanelId::Git));
    let expanded = render(&app, 130, 44);
    contains_all(&expanded, &["untracked", "last commit", "remote"]);
}

#[test]
fn plugin_manager_screen() {
    let mut app = temp_project(&[("main.rs", "fn main() {}\n")]);
    app.dispatch(Action::OpenPlugins);
    let out = render(&app, 130, 44);
    contains_all(&out, &["Plugin Manager", "loaded", "updates available"]);
}

// --- overlays --------------------------------------------------------------

#[test]
fn command_palette_filters() {
    let mut app = temp_project(&[("alpha.rs", "a\n"), ("Cargo.toml", "b\n")]);
    app.dispatch(Action::OpenPalette);
    let out = render(&app, 130, 44);
    contains_all(&out, &["Plugin Manager", "Toggle Sidebar"]);
    typ(&mut app, "carg");
    // Only "Cargo.toml" matches; other palette entries are filtered out.
    let filtered = render(&app, 130, 44);
    assert!(filtered.contains("Cargo.toml"));
    assert!(!filtered.contains("Toggle Sidebar"));
    assert!(!filtered.contains("Dashboard Layout"));
}

#[test]
fn explorer_overlay() {
    let mut app = temp_project(&[("main.rs", "fn main() {}\n"), ("README.md", "# hi\n")]);
    app.dispatch(Action::ToggleSidebar);
    let out = render(&app, 130, 44);
    contains_all(&out, &["EXPLORER", "README.md", "GIT"]);
}

#[test]
fn help_overlay() {
    let mut app = temp_project(&[("main.rs", "fn main() {}\n")]);
    app.dispatch(Action::ToggleHelp);
    let out = render(&app, 130, 44);
    contains_all(&out, &["Keybindings", "command palette", "insert mode"]);
}

#[test]
fn mouse_zones_are_registered() {
    let app = temp_project(&[("main.rs", "fn main() {}\n")]);
    let backend = TestBackend::new(130, 44);
    let mut term = Terminal::new(backend).unwrap();
    let mut zones = ui::Zones::default();
    term.draw(|f| {
        zones = ui::draw(f, &app);
    })
    .unwrap();
    assert!(!zones.0.is_empty(), "expected clickable zones to be registered");
}

// --- live editor (real backend) -------------------------------------------

#[test]
fn file_buffer_loads_real_file_through_engine() {
    let mut app = temp_project(&[("main.rs", "mod editor;\nmod buffer;\n")]);
    app.open_file(0);
    assert_eq!(app.engine.lines().first().map(String::as_str), Some("mod editor;"));
    let out = render(&app, 130, 44);
    contains_all(&out, &["mod editor;", "mod buffer;", "main.rs"]);
}

#[test]
fn editor_normal_mode_edits_through_engine() {
    let mut app = temp_project(&[("main.rs", "mod editor;\nmod buffer;\n")]);
    app.open_file(0);
    assert!(app.editor_focus());
    assert_eq!(app.editor_mode(), "n");
    key(&mut app, 'x'); // delete char under cursor
    assert_eq!(app.engine.lines()[0], "od editor;");
    key(&mut app, 'd'); // dd → delete line
    key(&mut app, 'd');
    assert_eq!(app.engine.lines()[0], "mod buffer;");
}

#[test]
fn editor_insert_mode_types_into_engine() {
    let mut app = temp_project(&[("f.rs", "hello\n")]);
    app.open_file(0);
    key(&mut app, 'i');
    assert_eq!(app.editor_mode(), "i");
    typ(&mut app, "XY");
    press(&mut app, KeyCode::Esc);
    assert_eq!(app.editor_mode(), "n");
    assert!(app.engine.lines()[0].starts_with("XYhello"));
    assert!(render(&app, 130, 44).contains("NORMAL"));
}

#[test]
fn edits_persist_across_buffer_switches() {
    let mut app = temp_project(&[("a.rs", "aaa\n"), ("b.rs", "bbb\n")]);
    app.open_file(0);
    key(&mut app, 'x'); // edit the first file
    let edited = app.engine.lines()[0].clone();
    app.open_file(1); // switch (snapshots the first)
    app.open_file(0); // back — edit preserved
    assert_eq!(app.engine.lines()[0], edited);
    assert_ne!(edited, "aaa"); // sanity: the edit actually happened
}

#[test]
fn palette_chord_escapes_the_editor() {
    let mut app = temp_project(&[("f.rs", "x\n")]);
    app.open_file(0);
    assert!(app.editor_focus());
    key(&mut app, ':'); // opens palette instead of editing
    assert!(app.palette_open);
    assert!(!app.editor_focus());
}

// --- live markdown rendering ----------------------------------------------

const DOC: &str = "para\n\n# Heading\n\n- item one\n- item two\n";

#[test]
fn markdown_files_live_render_by_default() {
    let mut app = temp_project(&[("doc.md", DOC)]);
    app.open_file(0);
    assert!(app.md_render_active(), "markdown should live-render on open");
    // Cursor is on line 0 ("para"), so every other line is concealed/rendered.
    let out = render(&app, 130, 44);
    contains_all(&out, &["Heading", "• item one", "item two"]);
    // The heading marker is concealed off the cursor line.
    assert!(!out.contains("# Heading"), "markup should be hidden:\n{out}");
}

#[test]
fn markdown_toggle_shows_raw_source() {
    let mut app = temp_project(&[("doc.md", DOC)]);
    app.open_file(0);
    ctrl(&mut app, 'g'); // toggle live rendering off
    assert!(!app.md_render_active());
    let out = render(&app, 130, 44);
    // Raw markup is visible again.
    contains_all(&out, &["# Heading", "- item one"]);
    ctrl(&mut app, 'g'); // back on
    assert!(app.md_render_active());
}

#[test]
fn markdown_cursor_line_stays_raw_and_editable() {
    let mut app = temp_project(&[("doc.md", DOC)]);
    app.open_file(0);
    key(&mut app, 'j'); // line 1 (blank)
    key(&mut app, 'j'); // line 2 ("# Heading") — now the cursor line
    assert_eq!(app.editor_cursor().0, 2);
    let out = render(&app, 130, 44);
    // The cursor's line reveals its markup so it can be edited...
    assert!(out.contains("# Heading"), "cursor line should be raw:\n{out}");
    // ...while another heading-free rendered line still shows its bullet.
    assert!(out.contains("• item one"));
}

#[test]
fn non_markdown_files_never_render() {
    let mut app = temp_project(&[("main.rs", "# not a heading\n")]);
    app.open_file(0);
    assert!(!app.md_render_active());
    ctrl(&mut app, 'g'); // no-op on non-markdown
    assert!(!app.md_render_active());
    assert!(render(&app, 130, 44).contains("# not a heading"));
}

// --- robustness ------------------------------------------------------------

#[test]
fn tiny_sizes_do_not_panic() {
    for (w, h) in [(1, 1), (2, 2), (5, 3), (10, 6), (20, 8), (40, 12), (80, 24)] {
        let mut app = temp_project(&[("main.rs", "fn main() {}\n")]);
        let _ = render(&app, w, h);
        app.dispatch(Action::SetLayout(Layout::Columns));
        let _ = render(&app, w, h);
        app.dispatch(Action::GotoSection(DashboardSection::Settings));
        let _ = render(&app, w, h);
        app.dispatch(Action::GotoSection(DashboardSection::About));
        let _ = render(&app, w, h);
        app.dispatch(Action::OpenPlugins);
        let _ = render(&app, w, h);
        app.open_file(0);
        let _ = render(&app, w, h);
        app.dispatch(Action::ToggleSidebar);
        let _ = render(&app, w, h);
        app.dispatch(Action::ToggleHelp);
        let _ = render(&app, w, h);
        app.dispatch(Action::OpenPalette);
        let _ = render(&app, w, h);
    }
}
