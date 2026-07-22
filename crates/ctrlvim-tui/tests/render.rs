//! Render-level smoke tests: drive the app through every screen/overlay and a
//! range of terminal sizes, asserting content appears and nothing panics.
//!
//! Panels backed by real project data (recent files, git, plugins, LSP, stats)
//! are exercised against a controlled temp directory so assertions are
//! deterministic; structural checks (headers, labels) cover the rest.

use std::fs;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use ctrlvim::app::{Action, App, DashboardSection, PanelId};
use ctrlvim::{input, ui};
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
fn dashboard_columns_default() {
    let app = temp_project(&[("main.rs", "fn main() {}\n")]);
    let out = render(&app, 130, 44);
    contains_all(
        &out,
        &[
            "ctrlvim",
            "workspace",
            "settings",
            "about",
            "ACTIONS",
            "New File",
            "Find Files",
            "RECENT FILES",
            "SESSIONS",
            "GIT STATUS",
            "STATS",
            "startup",
            "loc",
            "main.rs", // real file from the temp project
            "NORMAL",
        ],
    );
    // The tab-bar wordmark was removed.
    assert!(!out.contains("CHARVIM · TUI"));
    // The keybindings pane and the layout switcher are gone.
    assert!(!out.contains("KEYBINDINGS"));
    assert!(!out.contains("DASHBOARD LAYOUT"));
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
    // Known servers/linkers are always listed; install state varies by machine.
    contains_all(&out, &["LANGUAGE SERVERS", "rust_analyzer", "ts_ls", "jdtls", "mold", "lsp.toml"]);
    // The EDITOR options panel exposes the file-drawer setting.
    contains_all(&out, &["EDITOR", "Open file drawer on startup", "config.toml"]);
    assert!(!out.contains("┤ KEYBINDINGS ├")); // keybindings pane removed
}

#[test]
fn about_tab() {
    let mut app = temp_project(&[("main.rs", "fn main() {}\n")]);
    app.dispatch(Action::GotoSection(DashboardSection::About));
    let out = render(&app, 130, 44);
    contains_all(&out, &["ctrlvim", "a rust tui editor", "ratatui", "0.29", "crossterm", "MIT"]);
}

// --- panels / plugin manager ----------------------------------------------

#[test]
fn expand_git_reveals_more_when_repo() {
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
    app.config.drawer = true; // so the "Toggle Sidebar" command is listed
    app.dispatch(Action::OpenPalette);
    // The palette lists commands only — never files, even a `.toml`.
    let all: Vec<String> = app.palette_results().into_iter().map(|it| it.label).collect();
    assert!(all.iter().any(|l| l == "Plugin Manager"));
    assert!(all.iter().any(|l| l == "Toggle Sidebar"));
    assert!(!all.iter().any(|l| l.ends_with(".toml")), "no files in the palette: {all:?}");
    // Typing narrows to matching commands.
    typ(&mut app, "plug");
    let filtered: Vec<String> = app.palette_results().into_iter().map(|it| it.label).collect();
    assert!(filtered.iter().any(|l| l == "Plugin Manager"));
    assert!(!filtered.iter().any(|l| l == "Toggle Sidebar"));
}

#[test]
fn explorer_overlay() {
    let mut app = temp_project(&[("main.rs", "fn main() {}\n"), ("README.md", "# hi\n")]);
    app.config.drawer = true; // the drawer must be enabled to open
    app.dispatch(Action::ToggleSidebar);
    let out = render(&app, 130, 44);
    contains_all(&out, &["EXPLORER", "README.md", "GIT"]);
}

#[test]
fn disabled_drawer_cannot_be_opened_or_hinted() {
    let mut app = temp_project(&[("main.rs", "fn main() {}\n")]);
    assert!(!app.config.drawer, "drawer off by default");
    app.dispatch(Action::ToggleSidebar); // no-op while disabled
    assert!(!app.sidebar_visible);
    let out = render(&app, 130, 44);
    assert!(!out.contains("^B drawer"), "no drawer hint when disabled");
    // Enabling it makes the toggle and hint available.
    app.config.drawer = true;
    app.dispatch(Action::ToggleSidebar);
    assert!(app.sidebar_visible);
}

#[test]
fn help_overlay() {
    let mut app = temp_project(&[("main.rs", "fn main() {}\n")]);
    app.dispatch(Action::ToggleHelp);
    let out = render(&app, 130, 44);
    contains_all(&out, &["Keybindings", "command palette", "fuzzy file browser"]);
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
fn visual_mode_highlights_the_selection() {
    let mut app = temp_project(&[("f.rs", "hello world\n")]);
    app.open_file(0);
    key(&mut app, 'v'); // visual mode, anchored at col 0
    key(&mut app, 'l'); // extend right — selects "he"
    key(&mut app, 'l'); //                 selects "hel"
    assert_eq!(app.editor_mode(), "v");

    // Render into a raw backend so we can inspect cell backgrounds.
    let backend = TestBackend::new(130, 44);
    let mut term = Terminal::new(backend).unwrap();
    term.draw(|f| {
        ui::draw(f, &app);
    })
    .unwrap();
    let buf = term.backend().buffer().clone();

    // Find the "hello world" row and the x of its first char. The check is
    // theme-agnostic: the selection band cells share one background, distinct
    // from an unselected text cell on the same row.
    let mut hx = None;
    let mut hy = 0u16;
    for y in 0..buf.area.height {
        let mut row = String::new();
        for x in 0..buf.area.width {
            row.push_str(buf[(x, y)].symbol());
        }
        if let Some(byte) = row.find("hello world") {
            hx = Some(byte as u16);
            hy = y;
            break;
        }
    }
    let hx = hx.expect("the buffer text should be on screen");
    // 'h' and 'e' (cols 0,1) are selected; the cursor sits on 'l' (col 2) and
    // 'r'/'d' further right are unselected.
    let sel_bg = buf[(hx, hy)].style().bg; // 'h'
    assert!(sel_bg.is_some(), "selected cell should have a background");
    assert_eq!(sel_bg, buf[(hx + 1, hy)].style().bg, "band shares one background");
    let unselected_bg = buf[(hx + 7, hy)].style().bg; // 'o' in "world"
    assert_ne!(sel_bg, unselected_bg, "selection background must stand out from the text");
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
fn colon_opens_command_palette() {
    let mut app = temp_project(&[("f.rs", "x\n")]);
    app.open_file(0);
    assert!(app.editor_focus());
    key(&mut app, ':'); // `:` opens the unified command palette, not a raw cmdline
    assert!(app.palette_open);
    assert_eq!(app.engine.cmdline(), None); // engine stays in Normal mode
    // The palette lists commands (and only commands — no files).
    let labels: Vec<String> = app.palette_results().into_iter().map(|it| it.label).collect();
    assert!(labels.iter().any(|l| l == ":w"), "should list :w, got {labels:?}");
    assert!(labels.iter().any(|l| l.starts_with("Theme:")), "should list themes");
    assert!(!labels.iter().any(|l| l.ends_with(".rs")), "should not list files");
}

#[test]
fn palette_fuzzy_filters_commands() {
    let mut app = temp_project(&[("f.rs", "x\n")]);
    app.open_file(0);
    key(&mut app, ':');
    typ(&mut app, "wq"); // fuzzy query
    let labels: Vec<String> = app.palette_results().into_iter().map(|it| it.label).collect();
    assert!(labels.iter().any(|l| l == ":wq"), "':wq' should survive, got {labels:?}");
    assert!(!labels.iter().any(|l| l == ":q"), "':q' has no w→q subsequence, got {labels:?}");
    // Confirming closes the command line.
    press(&mut app, KeyCode::Enter);
    assert!(!app.palette_open, "palette closes after running a command");
}

#[test]
fn palette_surfaces_theme_commands() {
    let mut app = temp_project(&[("f.rs", "x\n")]);
    app.open_file(0);
    key(&mut app, ':');
    typ(&mut app, "gruvbox"); // fuzzy across "Theme: Gruvbox"
    let results = app.palette_results();
    let entry = results
        .iter()
        .find(|it| it.label == "Theme: Gruvbox")
        .expect("fuzzy 'gruvbox' should surface the Gruvbox theme");
    assert!(matches!(entry.action, Action::SetTheme(_)), "theme entries switch the theme");
}

#[test]
fn palette_runs_substitute_over_range() {
    let mut app = temp_project(&[("f.rs", "foo\nfoo\nfoo\n")]);
    app.open_file(0);
    // `:` opens the palette; a full ex command runs verbatim on submit.
    key(&mut app, ':');
    typ(&mut app, "%s/foo/bar/");
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.engine.lines(), vec!["bar", "bar", "bar"]);
}

#[test]
fn ex_vimscript_let_echo_and_setline() {
    let mut app = temp_project(&[("f.rs", "hello\n")]);
    app.open_file(0);
    run_cmd(&mut app, "let g:x = 41");
    run_cmd(&mut app, "echo g:x + 1"); // echo output → status message
    assert!(app.message.contains("42"), "echo output: {}", app.message);
    run_cmd(&mut app, "call setline(1, 'changed')"); // vimscript edits the buffer
    assert_eq!(app.engine.lines()[0], "changed");
}

#[test]
fn ex_lua_edits_buffer_and_reports_errors() {
    let mut app = temp_project(&[("f.rs", "before\n")]);
    app.open_file(0);
    run_cmd(&mut app, "lua vim.api.ctrlvim_set_current_line('lua edited')");
    assert_eq!(app.engine.lines()[0], "lua edited");
    // A Lua error surfaces on the command line.
    run_cmd(&mut app, "lua this is not lua(");
    assert!(app.message.contains("E5108"), "lua error: {}", app.message);
}

#[test]
fn hlsearch_highlights_matches() {
    let mut app = temp_project(&[("f.rs", "the cat sat\n")]);
    app.open_file(0);
    key(&mut app, '/'); // enter the search command line
    typ(&mut app, "cat");
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.editor_search_matches(0), vec![(4, 7)], "match cols for 'cat'");
    run_cmd(&mut app, "noh"); // :noh clears highlighting
    assert!(app.editor_search_matches(0).is_empty());
}

#[test]
fn slash_search_moves_cursor_through_engine() {
    let mut app = temp_project(&[("f.rs", "alpha\nbeta\nalpha\n")]);
    app.open_file(0);
    // `/` drops into the engine's search command line (rendered in the status
    // line); typing a pattern + Enter jumps to the next match.
    key(&mut app, '/');
    assert!(app.engine.cmdline().is_some());
    typ(&mut app, "beta");
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.editor_cursor().0, 1, "cursor jumps to the 'beta' line");
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
        app.dispatch(Action::OpenFinder);
        let _ = render(&app, w, h);
    }
}

// --- ex commands / leader / finder (engine-driven) -------------------------

/// Type a `:`-command in the editor and press Enter, via the real input layer.
fn run_cmd(app: &mut App, cmd: &str) {
    key(app, ':');
    typ(app, cmd);
    press(app, KeyCode::Enter);
}

#[test]
fn ex_write_persists_edits_to_disk() {
    let mut app = temp_project(&[("f.rs", "hello\n")]);
    let path = app.root.join("f.rs");
    app.open_file(0);
    key(&mut app, 'x'); // delete 'h' -> "ello"
    run_cmd(&mut app, "w");
    assert_eq!(app.engine.cmdline(), None); // command line closed
    let on_disk = std::fs::read_to_string(&path).unwrap();
    assert_eq!(on_disk, "ello\n");
}

#[test]
fn ex_quit_closes_buffer_then_app() {
    let mut app = temp_project(&[("a.rs", "a\n"), ("b.rs", "b\n")]);
    app.open_file(0); // Dashboard + a.rs
    assert_eq!(app.buffers.len(), 2);
    run_cmd(&mut app, "q"); // close a.rs -> back to just Dashboard
    assert_eq!(app.buffers.len(), 1);
    assert!(!app.should_quit);
    run_cmd(&mut app, "q"); // :q on the last window quits the app
    assert!(app.should_quit);
}

#[test]
fn ex_new_creates_and_opens_a_file() {
    let mut app = temp_project(&[("a.rs", "a\n")]);
    let path = app.root.join("fresh.rs");
    assert!(!path.exists());
    run_cmd(&mut app, "new fresh.rs"); // `:new <name>` from the command line
    assert!(path.exists(), "the file should be created on disk");
    assert!(app.is_file());
    assert_eq!(app.active_buffer().label, "fresh.rs");
}

#[test]
fn dashboard_n_starts_an_untitled_buffer() {
    let mut app = temp_project(&[("a.rs", "a\n")]);
    assert!(app.is_dashboard());
    key(&mut app, 'n'); // dashboard "new file" key
    // A fresh unnamed buffer opens for editing (no file browser, no disk file).
    assert!(app.is_file());
    assert_eq!(app.active_buffer().label, "[No Name]");
    assert!(app.active_buffer().path.is_none());
    assert!(app.finder.is_none());
}

#[test]
fn saving_an_unnamed_buffer_prompts_for_a_name() {
    let mut app = temp_project(&[("seed.rs", "x\n")]);
    app.dispatch(Action::NewFile); // untitled buffer
    key(&mut app, 'i');
    typ(&mut app, "hello");
    press(&mut app, KeyCode::Esc);
    run_cmd(&mut app, "w"); // :w on an unnamed buffer opens the save prompt
    assert!(app.save_prompt.is_some(), ":w should prompt for a name");
    typ(&mut app, "made.rs"); // routed to the save prompt
    press(&mut app, KeyCode::Enter);
    assert!(app.save_prompt.is_none());
    let path = app.root.join("made.rs");
    assert!(path.exists(), "the file is written under the typed name");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello\n");
    assert_eq!(app.active_buffer().label, "made.rs"); // buffer adopts the name
    assert!(!app.active_modified());
}

#[test]
fn finder_creates_a_file_for_an_unmatched_name() {
    let mut app = temp_project(&[("a.rs", "a\n")]);
    let path = app.root.join("brand-new.txt");
    app.dispatch(Action::OpenFinder);
    // Type a name that matches no existing entry, then confirm.
    for c in "brand-new.txt".chars() {
        app.finder_type(c);
    }
    assert!(app.finder_matches().is_empty(), "no existing entry matches");
    app.finder_select(); // Enter → create it here
    assert!(path.exists(), "the finder should create the typed file");
    assert!(app.finder.is_none());
    assert_eq!(app.active_buffer().label, "brand-new.txt");
}

#[test]
fn finder_colon_c_creates_and_opens_a_file() {
    let mut app = temp_project(&[("a.rs", "a\n")]);
    let path = app.root.join("made.txt");
    app.dispatch(Action::OpenFinder);
    typ(&mut app, ":c made.txt");
    press(&mut app, KeyCode::Enter);
    assert!(path.exists(), "`:c` should create the file");
    assert!(app.finder.is_none(), "creating a file closes the browser");
    assert_eq!(app.active_buffer().label, "made.txt");
}

#[test]
fn finder_colon_dir_creates_a_directory_and_stays_open() {
    let mut app = temp_project(&[("a.rs", "a\n")]);
    let path = app.root.join("newdir");
    app.dispatch(Action::OpenFinder);
    typ(&mut app, ":dir newdir");
    press(&mut app, KeyCode::Enter);
    assert!(path.is_dir(), "`:dir` should create the directory");
    let f = app.finder.as_ref().expect("browser stays open after :dir");
    assert!(f.query.is_empty(), "prompt is reset after the command");
    assert!(f.entries.iter().any(|e| e.name == "newdir/"), "listing refreshed");
}

#[test]
fn finder_colon_d_with_name_deletes() {
    let mut app = temp_project(&[("a.rs", "a\n"), ("gone.txt", "x\n")]);
    let path = app.root.join("gone.txt");
    app.dispatch(Action::OpenFinder);
    typ(&mut app, ":d gone.txt");
    press(&mut app, KeyCode::Enter);
    assert!(!path.exists(), "`:d <name>` should delete the file");
    assert!(app.finder.is_some(), "the browser stays open after delete");
}

#[test]
fn finder_colon_d_bare_deletes_highlighted_entry() {
    let mut app = temp_project(&[("only.rs", "a\n")]);
    let path = app.root.join("only.rs");
    app.dispatch(Action::OpenFinder);
    // `../` is pinned last; the sole file "only.rs" is highlighted at index 0.
    typ(&mut app, ":d");
    press(&mut app, KeyCode::Enter);
    assert!(!path.exists(), "bare `:d` should delete the highlighted entry");
}

#[test]
fn gt_and_dashboard_commands() {
    let mut app = temp_project(&[("a.rs", "a\n"), ("b.rs", "b\n"), ("c.rs", "c\n")]);
    app.open_file(0);
    app.open_file(1);
    app.open_file(2);
    app.set_active(1); // sit on a middle file tab (a.rs)
    typ(&mut app, "gt"); // next tab → b.rs (still a file)
    assert_eq!(app.active, 2);
    typ(&mut app, "gT"); // previous tab → a.rs
    assert_eq!(app.active, 1);
    // `:dash` returns to the dashboard.
    run_cmd(&mut app, "dash");
    assert!(app.is_dashboard());
    // `<leader>d` from the dashboard shell also works.
    app.open_file(0);
    typ(&mut app, " d");
    assert!(app.is_dashboard());
}

#[test]
fn mouse_scroll_is_opt_in() {
    let mut app = temp_project(&[("f.rs", "1\n2\n3\n4\n5\n6\n7\n8\n")]);
    app.open_file(0);
    assert_eq!(app.editor_cursor().0, 0);
    app.scroll_editor(3); // ignored while mouse support is off
    assert_eq!(app.editor_cursor().0, 0);
    app.config.mouse = true;
    app.scroll_editor(3); // now moves down 3 lines
    assert_eq!(app.editor_cursor().0, 3);
    app.scroll_editor(-2);
    assert_eq!(app.editor_cursor().0, 1);
}

#[test]
fn settings_navigation_spans_options_and_lsp() {
    let mut app = temp_project(&[("main.rs", "x\n")]);
    app.dispatch(Action::GotoSection(DashboardSection::Settings));
    assert_eq!(app.settings_index, 0); // drawer option
    app.move_settings(1);
    assert_eq!(app.settings_index, 1); // mouse option
    app.move_settings(1);
    assert_eq!(app.settings_index, 2, "j continues into the LSP list");
    // Toggling the focused LSP flips its enabled state (no disk write).
    let before = app.lsp_enabled[0];
    app.settings_toggle();
    assert_eq!(app.lsp_enabled[0], !before);
    // Wraps around from the last row back to the first.
    let last = app.settings_count() - 1;
    app.settings_index = last;
    app.move_settings(1);
    assert_eq!(app.settings_index, 0);
}

#[test]
fn ctrl_tab_cycles_buffers() {
    let mut app = temp_project(&[("a.rs", "a\n"), ("b.rs", "b\n")]);
    app.open_file(0);
    app.open_file(1); // dashboard + a.rs + b.rs, active = b.rs
    let start = app.active;
    input::handle_key(&mut app, KeyEvent::new(KeyCode::Tab, KeyModifiers::CONTROL));
    assert_ne!(app.active, start, "Ctrl+Tab moves to the next tab");
    let after_fwd = app.active;
    input::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Tab, KeyModifiers::CONTROL | KeyModifiers::SHIFT),
    );
    assert_ne!(app.active, after_fwd, "Ctrl+Shift+Tab moves back");
}

#[test]
fn leader_number_jumps_to_tab() {
    // Leader (Space) mappings run in the engine, so this works while a file tab
    // is focused. Tabs are 1-based: 1 = Dashboard, 2 = a.rs, 3 = b.rs, 4 = c.rs.
    let mut app = temp_project(&[("a.rs", "a\n"), ("b.rs", "b\n"), ("c.rs", "c\n")]);
    app.open_file(0);
    app.open_file(1);
    app.open_file(2); // active = c.rs (tab 4)
    typ(&mut app, " 2"); // <leader>2 → a.rs
    assert_eq!(app.active, 1);
    typ(&mut app, " 1"); // <leader>1 → Dashboard (tab 1)
    assert_eq!(app.active, 0);
    typ(&mut app, " 4"); // works from the dashboard too → c.rs
    assert_eq!(app.active, 3);
}

#[test]
fn ex_buffer_navigation() {
    let mut app = temp_project(&[("a.rs", "a\n"), ("b.rs", "b\n"), ("c.rs", "c\n")]);
    app.open_file(0); // dashboard + a.rs
    app.open_file(1); // + b.rs
    app.open_file(2); // + c.rs  (active)
    let last = app.active;
    run_cmd(&mut app, "bfirst");
    assert_eq!(app.active, 0, ":bfirst → first buffer");
    run_cmd(&mut app, "bnext");
    assert_eq!(app.active, 1, ":bnext advances");
    run_cmd(&mut app, "blast");
    assert_eq!(app.active, last, ":blast → last buffer");
    run_cmd(&mut app, "b 2"); // 1-based
    assert_eq!(app.active, 1);
    let before = app.buffers.len();
    run_cmd(&mut app, "bdelete");
    assert_eq!(app.buffers.len(), before - 1, ":bdelete closes a buffer");
}

#[test]
fn ex_only_closes_other_buffers() {
    let mut app = temp_project(&[("a.rs", "a\n"), ("b.rs", "b\n")]);
    app.open_file(0);
    app.open_file(1);
    assert!(app.buffers.len() >= 3); // dashboard + 2 files
    let active_label = app.active_buffer().label.clone();
    run_cmd(&mut app, "only");
    // Keeps the active buffer + the always-present (non-closable) dashboard.
    assert_eq!(app.buffers.len(), 2, ":only closes the other closable buffers");
    assert_eq!(app.active_buffer().label, active_label, "active buffer preserved");
}

#[test]
fn ex_colorscheme_reports_unknown() {
    // Happy-path switching persists to the state dir + mutates the process-wide
    // theme, so the integration layer only checks the safe error path here; the
    // theme roster/switching itself is unit-tested in `theme`.
    let mut app = temp_project(&[("a.rs", "a\n")]);
    app.open_file(0);
    let before = ctrlvim::theme::current().name;
    run_cmd(&mut app, "colorscheme definitely-not-a-theme");
    assert!(app.message.contains("E185"), "unknown scheme reports E185: {}", app.message);
    assert_eq!(ctrlvim::theme::current().name, before, "theme unchanged");
}

#[test]
fn ex_write_all_and_quit_all() {
    let mut app = temp_project(&[("a.rs", "a\n"), ("b.rs", "b\n")]);
    app.open_file(0);
    key(&mut app, 'x'); // modify a.rs
    assert!(app.active_modified());
    run_cmd(&mut app, "wa"); // write all
    assert!(!app.active_modified());
    run_cmd(&mut app, "qa"); // no unsaved changes → quits
    assert!(app.should_quit);
}

#[test]
fn ex_quit_all_blocks_on_unsaved() {
    let mut app = temp_project(&[("a.rs", "hello\n")]);
    app.open_file(0);
    key(&mut app, 'x'); // unsaved change
    run_cmd(&mut app, "qa"); // refused
    assert!(!app.should_quit);
    run_cmd(&mut app, "qa!"); // forced
    assert!(app.should_quit);
}

#[test]
fn ex_close_quits_the_app_directly() {
    let mut app = temp_project(&[("a.rs", "a\n"), ("b.rs", "b\n")]);
    app.open_file(0); // several buffers open
    assert!(app.buffers.len() > 1);
    run_cmd(&mut app, "close"); // `:close` exits ctrlvim regardless of buffer count
    assert!(app.should_quit);
}

#[test]
fn ex_quit_blocks_on_unsaved_changes() {
    let mut app = temp_project(&[("f.rs", "hello\n")]);
    app.open_file(0);
    key(&mut app, 'x'); // modify the buffer
    assert!(app.active_modified());
    run_cmd(&mut app, "q"); // refused — buffer stays open, app stays up
    assert!(!app.should_quit);
    assert_eq!(app.buffers.len(), 2);
    assert!(render(&app, 130, 44).contains("E37")); // error surfaced
    run_cmd(&mut app, "q!"); // force closes it
    assert_eq!(app.buffers.len(), 1);
}

#[test]
fn writing_clears_modified_marker() {
    let mut app = temp_project(&[("f.rs", "hello\n")]);
    app.open_file(0);
    key(&mut app, 'x');
    assert!(app.active_modified());
    assert!(render(&app, 130, 44).contains("[+]")); // dirty indicator
    run_cmd(&mut app, "w");
    assert!(!app.active_modified());
}

#[test]
fn ex_wq_writes_and_closes() {
    let mut app = temp_project(&[("f.rs", "hi\n")]);
    let path = app.root.join("f.rs");
    app.open_file(0);
    key(&mut app, 'x'); // "i"
    run_cmd(&mut app, "wq");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "i\n");
    // Only the Dashboard remains after the file closes.
    assert_eq!(app.buffers.len(), 1);
}

#[test]
fn leader_space_w_writes_through_engine_keymap() {
    let mut app = temp_project(&[("f.rs", "hello\n")]);
    let path = app.root.join("f.rs");
    app.open_file(0);
    key(&mut app, 'x'); // "ello"
    key(&mut app, ' '); // <leader>
    key(&mut app, 'w'); // <leader>w -> :w<CR>
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "ello\n");
}

#[test]
fn leader_space_e_opens_finder() {
    let mut app = temp_project(&[("f.rs", "x\n")]);
    app.open_file(0);
    key(&mut app, ' ');
    key(&mut app, 'e'); // <leader>e -> :Files<CR> -> OpenBrowser effect
    assert!(app.finder.is_some());
    let out = render(&app, 130, 44);
    contains_all(&out, &["File Browser", "f.rs"]);
}

#[test]
fn finder_lists_and_filters_directory() {
    let mut app = temp_project(&[("alpha.rs", "a\n"), ("zeta.md", "z\n")]);
    app.dispatch(Action::OpenFinder);
    let out = render(&app, 130, 44);
    contains_all(&out, &["alpha.rs", "zeta.md", "../"]);
    // Typing filters the listing. (Assert on finder state, not the rendered
    // string — the popup no longer covers the dashboard behind it.)
    for c in "alph".chars() {
        input::handle_key(&mut app, KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
    let names: Vec<String> = {
        let f = app.finder.as_ref().unwrap();
        app.finder_matches().iter().map(|&i| f.entries[i].name.clone()).collect()
    };
    assert!(names.iter().any(|n| n == "alpha.rs"), "got {names:?}");
    assert!(!names.iter().any(|n| n == "zeta.md"), "got {names:?}");
}

#[test]
fn drawer_slash_search_filters() {
    let mut app = temp_project(&[("alpha.rs", "a\n"), ("zeta.md", "z\n")]);
    app.config.drawer = true;
    app.dispatch(Action::ToggleSidebar);
    key(&mut app, '/'); // enter search
    assert!(app.drawer_search);
    typ(&mut app, "zet");
    let out = render(&app, 130, 44);
    assert!(out.contains("zeta.md"));
    assert!(!out.contains("alpha.rs"));
}
