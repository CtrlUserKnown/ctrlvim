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
use ctrlvim_core::MapMode;
use ctrlvim::{icons, input, ui};
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
    let mut app = App::with_root(dir, Instant::now());
    // `App::with_root` loads the *developer's* `~/.config/ctrlvim/config.toml`,
    // which would make these tests depend on whoever runs them — a machine with
    // `drawer = true` renders a different screen. Pin the defaults instead.
    app.config = ctrlvim::config::Config::default();
    app
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

/// Render and keep the cell grid, for assertions about *styling* rather than
/// text (syntax highlighting, selections).
fn render_cells(app: &App, w: u16, h: u16) -> ratatui::buffer::Buffer {
    let backend = TestBackend::new(w, h);
    let mut term = Terminal::new(backend).unwrap();
    term.draw(|f| {
        ui::draw(f, app);
    })
    .unwrap();
    term.backend().buffer().clone()
}

/// The foreground color the cell holding `needle` was drawn with, searching the
/// rendered grid row by row (the offset within `needle` lets a test pick a
/// specific character of the match).
fn fg_of(buf: &ratatui::buffer::Buffer, needle: &str, offset: u16) -> ratatui::style::Color {
    for y in 0..buf.area.height {
        let row: String = (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect();
        if let Some(byte) = row.find(needle) {
            let col = row[..byte].chars().count() as u16 + offset;
            return buf[(col, y)].fg;
        }
    }
    panic!("{needle:?} never rendered");
}

/// A 40-line file buffer, opened — enough to scroll around in.
fn long_file() -> App {
    let src: String = (1..=40).map(|i| format!("line {i:02}\n")).collect();
    let mut app = temp_project(&[("long.rs", &src)]);
    app.open_file(0);
    app
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
fn file_icons_use_glyphs_or_fall_back_to_a_letter() {
    let mut app = temp_project(&[("alpha.rs", "a\n"), ("zeta.md", "z\n")]);

    // With a Nerd Font, the chip is the glyph for the file type.
    app.config.icons = icons::IconMode::Nerd;
    let out = render(&app, 130, 44);
    contains_all(&out, &["\u{e7a8}", "\u{e73e}"]); // rust, markdown

    // Without one, it falls back to a letter in the same colored box.
    app.config.icons = icons::IconMode::Text;
    let out = render(&app, 130, 44);
    contains_all(&out, &[" R  alpha.rs", " M  zeta.md"]);
    assert!(!out.contains('\u{e7a8}'), "no glyphs when falling back to text");
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

#[test]
fn plugin_manager_shows_commands_a_matching_plugin_contributed() {
    use ctrlvim::model::{Plugin, PluginStatus};
    let mut app = temp_project(&[("main.rs", "fn main() {}\n")]);
    // Simulate a pack-directory plugin entry ...
    app.project.plugins.push(Plugin {
        name: "hello".to_string(),
        repo: "someone/hello".to_string(),
        category: "start".to_string(),
        status: PluginStatus::Loaded,
    });
    // ... whose startup script (file stem "hello") registered a command.
    app.engine.run_lua_as(Some("hello"), "vim.api.ctrlvim_create_user_command('Greet', function() end, {})").unwrap();
    app.dispatch(Action::OpenPlugins);
    let out = render(&app, 130, 44);
    contains_all(&out, &["hello", "commands: Greet"]);
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
    // Descriptions come from the live mapping table, not a hardcoded list.
    contains_all(&out, &["Keybindings", "command palette", "fuzzy file browser"]);
}

#[test]
fn help_overlay_lists_mappings_from_the_live_table() {
    // The modal used to render a hardcoded array and could not see user
    // mappings at all — its render fn didn't even take `app`.
    let mut app = temp_project(&[("main.rs", "fn main() {}\n")]);
    app.engine
        .session
        .keymap
        .set_with_desc(MapMode::Normal, "<leader>z", ":w<CR>", Some("zap it".into()))
        .unwrap();
    app.dispatch(Action::ToggleHelp);
    contains_all(&render(&app, 130, 44), &["<Space>z", "zap it"]);
}

#[test]
fn help_overlay_drops_a_mapping_that_was_unmapped() {
    let mut app = temp_project(&[("main.rs", "fn main() {}\n")]);
    app.dispatch(Action::ToggleHelp);
    assert!(render(&app, 130, 44).contains("find & replace in project"));

    assert!(app.engine.session.keymap.remove(MapMode::Normal, "<leader>S"));
    assert!(
        !render(&app, 130, 44).contains("find & replace in project"),
        "a removed mapping must stop being advertised"
    );
}

#[test]
fn a_mapping_with_no_desc_falls_back_to_its_rhs() {
    // A described mapping is better, but an undescribed one must still be
    // listed — a blank row would be worse than a slightly technical one.
    let mut app = temp_project(&[("main.rs", "fn main() {}\n")]);
    app.engine.session.keymap.set_normal("<leader>z", ":wq<CR>").unwrap();
    app.dispatch(Action::ToggleHelp);
    contains_all(&render(&app, 130, 44), &["<Space>z", ":wq<CR>"]);
}

#[test]
fn a_config_mapping_works_on_the_dashboard_too() {
    // The dashboard used to run a hand-rolled leader machine that knew only
    // `<leader>1-9`, `<leader>d` and `<leader>S`, so a user's `[[keymap]]`
    // entries were silently editor-only.
    let mut app = temp_project(&[("a.rs", "a\n")]);
    app.engine
        .session
        .keymap
        .set_normal("<leader>z", ":Files<CR>")
        .unwrap();
    assert_eq!(app.active, 0, "on the dashboard, not a file buffer");
    assert!(app.finder.is_none());

    typ(&mut app, " z");
    assert!(app.finder.is_some(), "the user's own chord ran the file browser");
}

#[test]
fn an_unmapped_shell_key_still_reaches_the_dashboard_keys() {
    // Routing shell keys through the mapping table must not swallow the
    // dashboard's own navigation.
    let mut app = temp_project(&[("a.rs", "a\n")]);
    key(&mut app, 'p');
    assert!(
        matches!(app.buffers[app.active].kind, ctrlvim::app::BufferKind::Plugins),
        "`p` still opens the plugin manager"
    );
}

#[test]
fn which_key_popup_lists_what_can_follow_a_pending_chord() {
    let mut app = temp_project(&[("main.rs", "fn main() {}\n")]);
    app.open_file(0);
    assert!(!render(&app, 130, 44).contains("write the buffer"), "nothing pending yet");

    key(&mut app, ' '); // half-type the leader chord
    let out = render(&app, 130, 44);
    contains_all(&out, &["write the buffer", "find & replace in project"]);
}

#[test]
fn which_key_popup_clears_once_the_chord_resolves() {
    let mut app = temp_project(&[("main.rs", "fn main() {}\n")]);
    app.open_file(0);
    key(&mut app, ' ');
    assert!(render(&app, 130, 44).contains("write the buffer"));
    key(&mut app, 'd'); // `<leader>d` — completes the chord
    assert!(
        !render(&app, 130, 44).contains("write the buffer"),
        "the popup must not outlive the chord it was describing"
    );
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
fn palette_surfaces_user_defined_commands() {
    let mut app = temp_project(&[("f.rs", "hello\n")]);
    app.open_file(0);
    run_cmd(&mut app, "command Greet echo 'hi'"); // :command Name expansion
    key(&mut app, ':');
    typ(&mut app, "Greet");
    let results = app.palette_results();
    let idx = results
        .iter()
        .position(|it| it.label == ":Greet")
        .expect("a :command-defined name should show up in the palette");
    assert!(matches!(&results[idx].action, Action::RunEx(name) if name == "Greet"));
    app.palette_index = idx;
    press(&mut app, KeyCode::Enter);
    assert!(app.message.contains("hi"), "selecting it should run the expansion: {}", app.message);
}

#[test]
fn palette_surfaces_plugin_registered_commands() {
    let mut app = temp_project(&[("f.rs", "hello\n")]);
    app.open_file(0);
    run_cmd(
        &mut app,
        "lua vim.api.ctrlvim_create_user_command('Shout', function() \
         vim.api.ctrlvim_set_current_line(vim.api.ctrlvim_get_current_line():upper()) \
         end, { desc = 'uppercase the line' })",
    );
    key(&mut app, ':');
    typ(&mut app, "Shout");
    let results = app.palette_results();
    let idx = results
        .iter()
        .position(|it| it.label == ":Shout")
        .expect("a plugin-registered command should show up in the palette");
    assert_eq!(results[idx].hint, "uppercase the line");
    assert!(matches!(&results[idx].action, Action::RunPluginCommand(name) if name == "Shout"));
    app.palette_index = idx;
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.engine.lines(), vec!["HELLO"]);
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
fn rust_buffers_are_syntax_highlighted_by_the_engine() {
    let src = "// a note\nfn main() {\n    let s = \"hi\";\n}\n";
    let mut app = temp_project(&[("main.rs", src)]);
    app.open_file(0);
    let buf = render_cells(&app, 100, 24);

    // Each class is drawn in the theme's own palette, so the assertions are
    // against the theme rather than hard-coded colors.
    // Offset 3, not 0: the cursor sits on the first cell and inverts it.
    assert_eq!(fg_of(&buf, "// a note", 3), ctrlvim::theme::fg_dim(), "comment");
    assert_eq!(fg_of(&buf, "fn main", 0), ctrlvim::theme::purple(), "`fn` keyword");
    assert_eq!(fg_of(&buf, "fn main", 3), ctrlvim::theme::blue(), "function name");
    assert_eq!(fg_of(&buf, "\"hi\"", 0), ctrlvim::theme::green(), "string");
    // The status line reports the filetype the highlighter actually used.
    assert!(render(&app, 100, 24).contains("rust"));
}

#[test]
fn unsupported_filetypes_render_plain() {
    let mut app = temp_project(&[("notes.txt", "fn main() {}\n")]);
    app.open_file(0);
    assert!(app.editor_filetype().is_none(), "no grammar for .txt");
    let buf = render_cells(&app, 100, 24);
    // The same `fn` that is a keyword in Rust stays default-colored here
    // (offset 1: the cursor inverts the first cell).
    assert_eq!(fg_of(&buf, "fn main", 1), ctrlvim::theme::fg());
}

#[test]
fn highlights_follow_edits() {
    let mut app = temp_project(&[("main.rs", "let x = 1;\n")]);
    app.open_file(0);
    // Type a comment marker at the start of the line: the whole line becomes a
    // comment, which only holds if the cache was invalidated by the edit.
    key(&mut app, 'i');
    typ(&mut app, "// ");
    let buf = render_cells(&app, 100, 24);
    assert_eq!(fg_of(&buf, "// let x", 4), ctrlvim::theme::fg_dim(), "now a comment");
}

// --- tags ------------------------------------------------------------------

/// A project with two source files and a `tags` file pointing into them, the
/// way `ctags -R .` would leave it.
fn tagged_project() -> App {
    let tags = "\
!_TAG_FILE_FORMAT\t2\t/extended format/
Helper\tlib.rs\t/^pub struct Helper {$/;\"\ts
helper\tlib.rs\t/^pub fn helper() {}$/;\"\tf
helper\talt.rs\t2;\"\tf
";
    let mut app = temp_project(&[
        ("main.rs", "fn main() {\n    helper();\n}\n"),
        ("lib.rs", "// lib\npub struct Helper {\n}\npub fn helper() {}\n"),
        ("alt.rs", "// alt\npub fn helper() {}\n"),
        ("tags", tags),
    ]);
    // main.rs is the file the cursor starts in.
    let idx = app
        .project
        .recent_files
        .iter()
        .position(|f| f.name == "main.rs")
        .expect("main.rs in the project");
    app.open_file(idx);
    app
}

/// `Ctrl-]` as the terminal delivers it.
fn ctrl_key(app: &mut App, c: char) {
    input::handle_key(app, KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL));
}

#[test]
fn ctrl_bracket_jumps_to_the_definition() {
    let mut app = tagged_project();
    // Put the cursor on `helper` in `    helper();`.
    key(&mut app, 'j');
    typ(&mut app, "fh");
    ctrl_key(&mut app, ']');

    assert_eq!(app.active_buffer().label, "lib.rs", "jumped to the defining file");
    assert_eq!(app.editor_cursor().0, 3, "onto the `pub fn helper` line");
}

#[test]
fn ctrl_t_returns_to_where_the_jump_started() {
    let mut app = tagged_project();
    key(&mut app, 'j');
    typ(&mut app, "fh");
    ctrl_key(&mut app, ']');
    assert_eq!(app.active_buffer().label, "lib.rs");

    ctrl_key(&mut app, 't');
    assert_eq!(app.active_buffer().label, "main.rs", "back to the original file");
    assert_eq!(app.editor_cursor().0, 1, "and the original line");
}

#[test]
fn a_pattern_address_finds_a_definition_that_moved() {
    let mut app = tagged_project();
    // Insert a line at the top of lib.rs so the definition shifts down.
    let lib = app.root.join("lib.rs");
    let text = std::fs::read_to_string(&lib).unwrap();
    std::fs::write(&lib, format!("// added\n{}", text)).unwrap();

    key(&mut app, 'j');
    typ(&mut app, "fh");
    ctrl_key(&mut app, ']');
    assert_eq!(app.active_buffer().label, "lib.rs");
    assert_eq!(app.editor_cursor().0, 4, "the pattern tracked the shifted line");
}

#[test]
fn an_unknown_identifier_reports_rather_than_jumping() {
    let mut app = tagged_project();
    typ(&mut app, "fm"); // `main`, which is not in the tags file
    ctrl_key(&mut app, ']');
    assert_eq!(app.active_buffer().label, "main.rs", "stayed put");
    assert!(app.message.contains("E426"), "got {:?}", app.message);
}

#[test]
fn tnext_walks_a_name_with_two_definitions() {
    let mut app = tagged_project();
    key(&mut app, 'j');
    typ(&mut app, "fh");
    ctrl_key(&mut app, ']');
    assert_eq!(app.active_buffer().label, "lib.rs");
    assert!(app.message.contains("1 of 2"), "got {:?}", app.message);

    typ(&mut app, ":tnext");
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.active_buffer().label, "alt.rs", "second definition");
    assert!(app.message.contains("2 of 2"), "got {:?}", app.message);
}

#[test]
fn a_regenerated_tags_file_is_picked_up_without_a_reload() {
    let mut app = tagged_project();
    // Rewrite the tags file to point `helper` somewhere else entirely.
    std::fs::write(
        app.root.join("tags"),
        "helper\talt.rs\t/^pub fn helper() {}$/;\"\tf\n",
    )
    .unwrap();
    // Ensure the mtime differs even on a coarse-grained filesystem.
    let later = std::time::SystemTime::now() + std::time::Duration::from_secs(2);
    let _ = filetime_set(&app.root.join("tags"), later);

    key(&mut app, 'j');
    typ(&mut app, "fh");
    ctrl_key(&mut app, ']');
    assert_eq!(app.active_buffer().label, "alt.rs", "used the new tags file");
}

/// Nudge a file's mtime forward (no external crate; best-effort).
fn filetime_set(path: &std::path::Path, when: std::time::SystemTime) -> std::io::Result<()> {
    let file = fs::OpenOptions::new().write(true).open(path)?;
    file.set_modified(when)
}

#[test]
fn tags_with_no_tags_file_reports_clearly() {
    let mut app = temp_project(&[("main.rs", "fn main() { helper(); }\n")]);
    app.open_file(0);
    typ(&mut app, "fh");
    ctrl_key(&mut app, ']');
    assert!(app.message.contains("E433"), "got {:?}", app.message);
}

// --- folds -----------------------------------------------------------------

/// A file buffer of `fn` blocks, opened and ready for `z` commands.
fn foldable_project() -> App {
    let src = "fn one() {\n    a;\n    b;\n}\nfn two() {\n    c;\n}\nlast\n";
    let mut app = temp_project(&[("main.rs", src)]);
    app.open_file(0);
    app
}

#[test]
fn a_closed_fold_draws_one_summary_row_and_hides_the_rest() {
    let mut app = foldable_project();
    typ(&mut app, "zf3j"); // fold lines 1..=4 (0-based 0..=3)
    let out = render(&app, 100, 30);
    contains_all(&out, &["4 lines: fn one() {"]);
    assert!(!out.contains("    a;"), "the fold's body is hidden:\n{out}");
    // Lines after the fold are still there, moved up.
    contains_all(&out, &["fn two() {", "last"]);
}

#[test]
fn opening_the_fold_brings_the_lines_back() {
    let mut app = foldable_project();
    typ(&mut app, "zf3j");
    assert!(!render(&app, 100, 30).contains("    a;"));
    typ(&mut app, "zo");
    let out = render(&app, 100, 30);
    assert!(out.contains("    a;"));
    assert!(!out.contains("4 lines:"), "no summary row once open:\n{out}");
}

#[test]
fn line_numbers_stay_with_their_lines_across_a_fold() {
    let mut app = foldable_project();
    typ(&mut app, "zf3j");
    let out = render(&app, 100, 30);
    // The row after the summary is buffer line 5, and keeps its own number —
    // the gutter numbers buffer lines, not screen rows.
    let row = out
        .lines()
        .find(|l| l.contains("fn two()"))
        .expect("fn two() should render");
    assert!(row.trim_start().starts_with('5'), "expected line number 5 in {row:?}");
}

#[test]
fn the_cursor_never_lands_inside_a_closed_fold() {
    let mut app = foldable_project();
    typ(&mut app, "zf3j");
    assert_eq!(app.editor_cursor().0, 0);
    key(&mut app, 'j');
    assert_eq!(app.editor_cursor().0, 4, "one press clears the whole fold");
    // And the rendered cursor is on a row that exists.
    let out = render(&app, 100, 30);
    assert!(out.contains("fn two() {"));
}

#[test]
fn syntax_highlighting_follows_the_lines_a_fold_shifts() {
    let mut app = foldable_project();
    app.config.icons = icons::IconMode::Text; // keep the chip out of the way
    typ(&mut app, "zf3j");
    let buf = render_cells(&app, 100, 30);
    // `fn` on the *shifted* row is still a keyword: the highlight span lookup
    // must index by buffer line, not screen row.
    assert_eq!(fg_of(&buf, "fn two", 0), ctrlvim::theme::purple());
}

#[test]
fn folds_survive_scrolling_past_them() {
    // A buffer taller than the viewport, with a fold above the visible area.
    let mut src = String::from("fn head() {\n    x;\n    y;\n}\n");
    for i in 0..60 {
        src.push_str(&format!("line {i}\n"));
    }
    let mut app = temp_project(&[("big.rs", &src)]);
    app.open_file(0);
    typ(&mut app, "zf3j"); // fold the first 4 lines
    typ(&mut app, "G"); // jump to the end
    let out = render(&app, 100, 20);
    assert!(out.contains("line 59"), "the end of the buffer is visible:\n{out}");
    assert!(!out.contains("4 lines: fn head"), "the fold scrolled off the top");
}

#[test]
fn set_foldmethod_indent_folds_without_zf() {
    let mut app = foldable_project();
    typ(&mut app, ":set shiftwidth=4");
    press(&mut app, KeyCode::Enter);
    typ(&mut app, ":set foldmethod=indent");
    press(&mut app, KeyCode::Enter);
    assert!(!app.folds().is_empty(), "indent derived some folds");
    // Derived folds start open, so nothing is hidden until zM.
    assert!(render(&app, 100, 30).contains("    a;"));
    typ(&mut app, "zM");
    let out = render(&app, 100, 30);
    assert!(!out.contains("    a;"), "zM closed them:\n{out}");
}

// --- quickfix --------------------------------------------------------------

/// Run `:vimgrep` over a temp project the way a user would type it.
fn vimgrep(app: &mut App, cmd: &str) {
    typ(app, cmd);
    press(app, KeyCode::Enter);
}

#[test]
fn vimgrep_fills_the_quickfix_list_and_opens_the_pane() {
    let mut app = temp_project(&[
        ("a.rs", "fn one() {}\nlet x = 1;\n"),
        ("b.rs", "fn two() {}\n"),
        ("notes.txt", "fn in a text file\n"),
    ]);
    app.open_file(0);
    vimgrep(&mut app, ":vimgrep /fn / *.rs");

    let qf = app.engine.session.quickfix();
    assert_eq!(qf.len(), 2, "two Rust hits; the .txt is excluded by the glob");
    assert!(app.quickfix_open, "a non-empty result opens the pane");

    let out = render(&app, 120, 40);
    contains_all(&out, &["a.rs:1", "b.rs:1", "fn one() {}", "entries"]);
}

#[test]
fn vimgrep_with_no_matches_reports_instead_of_opening() {
    let mut app = temp_project(&[("a.rs", "fn one() {}\n")]);
    app.open_file(0);
    vimgrep(&mut app, ":vimgrep /nonexistent/");
    assert!(app.engine.session.quickfix().is_empty());
    assert!(!app.quickfix_open, "an empty result must not open an empty pane");
    assert!(app.message.contains("no matches"), "got {:?}", app.message);
}

// --- `:!{cmd}` shell overlay -------------------------------------------------

#[test]
fn bang_command_runs_through_the_configured_shell_and_shows_output() {
    let mut app = temp_project(&[("a.rs", "fn one() {}\n")]);
    typ(&mut app, ":!echo hello-ctrlvim");
    press(&mut app, KeyCode::Enter);

    let deadline = Instant::now() + std::time::Duration::from_secs(5);
    while !app.shell_open && Instant::now() < deadline {
        app.poll_jobs();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    assert!(app.shell_open, "the overlay should open once the job exits");
    assert!(
        app.shell_output.iter().any(|l| l.contains("hello-ctrlvim")),
        "got {:?}",
        app.shell_output
    );
    assert!(app.shell_title.contains("exit 0"), "got {:?}", app.shell_title);

    let out = render(&app, 120, 40);
    assert!(out.contains("hello-ctrlvim"), "output should render in the overlay:\n{out}");

    press(&mut app, KeyCode::Esc);
    assert!(!app.shell_open, "Esc dismisses the overlay");
}

#[test]
fn bang_output_containing_tabs_is_expanded_before_display() {
    // Real subprocess output (e.g. `git status`) is often tab-indented. A raw
    // tab byte reaching the terminal is interpreted differently by different
    // emulators — some jump the real cursor to a tab stop instead of treating
    // it as a printable cell — which desyncs ratatui's layout from the actual
    // screen. ctrlvim must expand tabs to spaces itself rather than ever
    // emitting one.
    let mut app = temp_project(&[("a.rs", "fn one() {}\n")]);
    typ(&mut app, ":!printf 'a\\tb\\n'");
    press(&mut app, KeyCode::Enter);

    let deadline = Instant::now() + std::time::Duration::from_secs(5);
    while !app.shell_open && Instant::now() < deadline {
        app.poll_jobs();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    assert!(app.shell_open, "the overlay should open once the job exits");
    assert!(
        !app.shell_output.iter().any(|l| l.contains('\t')),
        "no raw tab byte should reach a rendered line: {:?}",
        app.shell_output
    );
    let expanded = format!("a{}b", " ".repeat(7)); // 'a' then a tab out to column 8
    assert!(
        app.shell_output.iter().any(|l| l.contains(&expanded)),
        "tab should expand to a column stop: {:?}",
        app.shell_output
    );
}

#[test]
fn bare_bang_with_no_command_reports_an_error_rather_than_running_the_shell() {
    let mut app = temp_project(&[("a.rs", "fn one() {}\n")]);
    typ(&mut app, ":!");
    press(&mut app, KeyCode::Enter);
    assert!(!app.shell_open);
    assert!(app.message.contains("E34"), "got {:?}", app.message);
}

#[test]
fn an_invalid_pattern_reports_rather_than_panicking() {
    let mut app = temp_project(&[("a.rs", "x\n")]);
    app.open_file(0);
    // An unclosed group, not an unclosed `[` — Vim reads a bare bracket as a
    // literal, so it is a valid pattern rather than an error.
    vimgrep(&mut app, r":vimgrep /\(unclosed/");
    assert!(app.message.contains("E486"), "got {:?}", app.message);
}

#[test]
fn cnext_walks_the_list_and_opens_the_right_file_and_line() {
    let mut app = temp_project(&[("a.rs", "one\nTARGET here\n"), ("b.rs", "TARGET again\n")]);
    app.open_file(0);
    vimgrep(&mut app, ":vimgrep /TARGET/");
    assert_eq!(app.engine.session.quickfix().len(), 2);

    // The first entry is selected but not jumped to until :cc/:cnext.
    vimgrep(&mut app, ":cc 1");
    let first = app.active_buffer().label.clone();
    assert_eq!(app.editor_cursor().0, 1, "a.rs match is on line 2 (0-based 1)");

    vimgrep(&mut app, ":cnext");
    assert_ne!(app.active_buffer().label, first, "moved to the other file");
    assert_eq!(app.editor_cursor().0, 0, "b.rs match is on line 1");
}

#[test]
fn quickfix_rows_are_clickable() {
    let mut app = temp_project(&[("a.rs", "hit one\nhit two\n")]);
    app.open_file(0);
    vimgrep(&mut app, ":vimgrep /hit/");

    let backend = TestBackend::new(120, 40);
    let mut term = Terminal::new(backend).unwrap();
    let mut zones = ui::Zones::default();
    term.draw(|f| zones = ui::draw(f, &app)).unwrap();

    // Find the zone for the second entry and dispatch it, as a click would.
    let hit = (0..40)
        .flat_map(|y| (0..120).map(move |x| (x, y)))
        .find_map(|(x, y)| match zones.hit(x, y) {
            Some(Action::QuickfixSelect(1)) => Some(()),
            _ => None,
        });
    assert!(hit.is_some(), "the second quickfix row registered no click zone");

    app.dispatch(Action::QuickfixSelect(1));
    assert_eq!(app.editor_cursor().0, 1, "clicking the row jumped to its line");
}

#[test]
fn closing_the_pane_gives_the_space_back_to_the_editor() {
    let mut app = temp_project(&[("a.rs", "hit\n")]);
    app.open_file(0);
    vimgrep(&mut app, ":vimgrep /hit/");
    // `a.rs:1` is a pane row — the status message mentions the file but never
    // in `path:line` form.
    assert!(render(&app, 120, 40).contains("a.rs:1"));
    vimgrep(&mut app, ":cclose");
    assert!(!app.quickfix_open);
    assert!(!render(&app, 120, 40).contains("a.rs:1"));
}

#[test]
fn globs_select_files_the_way_vimgrep_expects() {
    use ctrlvim::app::glob_match;
    assert!(glob_match("src/main.rs", "*.rs"), "a bare pattern matches the file name");
    assert!(glob_match("a/b/c/deep.rs", "**/*.rs"));
    assert!(glob_match("src/main.rs", "src/*.rs"));
    assert!(!glob_match("tests/main.rs", "src/*.rs"));
    assert!(!glob_match("src/a/b.rs", "src/*.rs"), "a single * stays inside one segment");
    assert!(glob_match("src/a/b.rs", "src/**/*.rs"));
    assert!(glob_match("src/main.rs", "**/*.rs"), "**/ also matches zero directories");
    assert!(glob_match("anything", ""), "an empty glob matches everything");
}

#[test]
fn markdown_rendering_wins_over_syntax_highlighting() {
    // Both decorate a buffer; markdown's live render owns the styling when on.
    let mut app = temp_project(&[("doc.md", DOC)]);
    app.open_file(0);
    assert!(app.md_render_active());
    assert!(app.editor_highlights(&app.editor_lines(), 0, 10).is_empty());
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
fn mouse_scroll_moves_the_view() {
    let mut app = long_file();
    assert!(app.config.mouse, "mouse scrolling is on by default");
    // One render so the app knows how tall the viewport is.
    let out = render(&app, 100, 20);
    assert!(out.contains("line 01"), "starts at the top:\n{out}");

    app.scroll_editor(5);
    let out = render(&app, 100, 20);
    assert!(!out.contains("line 01"), "scrolled past the first line:\n{out}");
    assert!(out.contains("line 06"), "now showing from line 6:\n{out}");
}

#[test]
fn scrolling_only_drags_the_cursor_when_it_would_leave_the_view() {
    let mut app = long_file();
    render(&app, 100, 20);

    // Scrolling down past the cursor pulls it along, since it may not sit
    // outside the window.
    app.scroll_editor(5);
    assert_eq!(app.editor_cursor().0, 5, "cursor followed the view down");

    // Scrolling back up while the cursor is still on screen leaves it alone —
    // this is view scrolling, not cursor movement.
    app.scroll_editor(-2);
    assert_eq!(app.editor_cursor().0, 5, "cursor stayed put");
    let out = render(&app, 100, 20);
    assert!(out.contains("line 04"), "but the view moved:\n{out}");
}

#[test]
fn scrolling_never_edits_the_buffer() {
    // Regression: scrolling used to feed `j`/`k` into the engine, so a wheel
    // tick with an operator pending (`d` then scroll) deleted lines.
    let mut app = long_file();
    render(&app, 100, 20);
    let before = app.editor_lines();
    key(&mut app, 'd'); // operator pending
    app.scroll_editor(3);
    app.scroll_editor(-3);
    assert_eq!(app.editor_lines(), before, "the buffer is untouched");
}

#[test]
fn scrolling_stops_at_the_buffer_edges() {
    let mut app = long_file();
    render(&app, 100, 20);
    app.scroll_editor(-10); // already at the top
    assert_eq!(app.view_top(), 0);
    app.scroll_editor(9999); // far past the end
    let out = render(&app, 100, 20);
    assert!(out.contains("line 40"), "the last line is still reachable:\n{out}");
}

#[test]
fn mouse_scrolling_can_be_turned_off() {
    let mut app = long_file();
    render(&app, 100, 20);
    app.config.mouse = false;
    app.scroll_editor(5);
    assert_eq!(app.view_top(), 0, "the wheel belongs to the terminal now");
}

#[test]
fn scrolling_a_folded_buffer_moves_by_screen_rows() {
    let mut app = long_file();
    typ(&mut app, "zf9j"); // fold lines 1..=10 into one row
    render(&app, 100, 20);
    app.scroll_editor(2);
    let out = render(&app, 100, 20);
    // Two rows past the fold's summary row is line 12, not line 3 — the
    // collapsed lines don't count as rows.
    assert!(out.contains("line 12"), "scrolled by screen rows:\n{out}");
}

#[test]
fn settings_navigation_spans_options_and_lsp() {
    let mut app = temp_project(&[("main.rs", "x\n")]);
    app.dispatch(Action::GotoSection(DashboardSection::Settings));
    assert_eq!(app.settings_index, 0); // drawer option
    app.move_settings(1);
    assert_eq!(app.settings_index, 1); // mouse option
    app.move_settings(1);
    assert_eq!(app.settings_index, 2); // file-icons option
    app.move_settings(1);
    assert_eq!(app.settings_index, 3); // inline AI suggestions
    app.move_settings(1);
    assert_eq!(app.settings_index, 4, "j continues into the LSP list");
    // Toggling the focused LSP flips its enabled state (no disk write). The
    // option rows above deliberately aren't toggled here: they persist to the
    // *real* `~/.config/ctrlvim/config.toml`, which a test must never touch.
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
fn installing_an_unsupported_tool_reports_a_message_without_spawning_a_job() {
    let mut app = temp_project(&[("main.rs", "x\n")]);
    // lua_ls has no scripted install method — whether or not it happens to be
    // installed on the machine running this test, no job should ever spawn.
    let i = app.project.lsp.iter().position(|l| l.name == "lua_ls").expect("lua_ls in the registry");
    app.install_tool(i);
    assert!(
        app.message.contains("already installed") || app.message.contains("no install method available"),
        "unexpected message: {}",
        app.message
    );
    assert!(!app.shell_open, "no install job should have been spawned");
}

#[test]
fn installing_the_focused_row_is_a_noop_on_an_editor_option() {
    let mut app = temp_project(&[("main.rs", "x\n")]);
    app.dispatch(Action::GotoSection(DashboardSection::Settings));
    assert_eq!(app.settings_index, 0); // an EDITOR option, not a tool row
    app.install_focused_tool();
    assert!(!app.shell_open, "installing an EDITOR option row should do nothing");
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
fn palette_runs_the_highlighted_command_on_a_short_prefix() {
    // Regression: ":cl" used to show ":close" highlighted in the palette but
    // execute the unrelated ":clist" abbreviation on Enter instead.
    let mut app = temp_project(&[("a.rs", "a\n"), ("b.rs", "b\n")]);
    app.open_file(0);
    key(&mut app, ':');
    typ(&mut app, "cl");
    let results = app.palette_results();
    assert_eq!(results[app.palette_index].label, ":close", "':close' should be highlighted, got {:?}", results.iter().map(|it| &it.label).collect::<Vec<_>>());
    press(&mut app, KeyCode::Enter);
    assert!(app.should_quit, "Enter should run the highlighted ':close', not a hidden ':clist' abbreviation");
}

#[test]
fn palette_short_prefix_still_prefers_exact_abbreviation_over_the_fuzzy_list() {
    // ":q" must still quit rather than running ":wq", even though "wq"
    // contains a "q" and would otherwise sort first in the fuzzy list.
    let mut app = temp_project(&[("a.rs", "a\n"), ("b.rs", "b\n")]);
    app.open_file(0);
    run_cmd(&mut app, "q"); // -> back to just Dashboard
    assert_eq!(app.buffers.len(), 1);
    assert!(!app.should_quit);
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

// --- find & replace panel --------------------------------------------------

/// A two-file project the replace tests share.
fn replace_project() -> App {
    temp_project(&[
        ("alpha.rs", "let widget = 1;\nfn other() {}\nprintln!(\"{widget} {widget}\");\n"),
        ("beta.rs", "use widget::thing;\n"),
    ])
}

/// The panel's hits as `path:line` strings, for order-sensitive assertions.
fn hit_locs(app: &App) -> Vec<String> {
    app.replace
        .as_ref()
        .unwrap()
        .hits
        .iter()
        .map(|h| format!("{}:{}", h.path.display(), h.line))
        .collect()
}

#[test]
fn replace_panel_searches_the_whole_project_on_open() {
    let mut app = replace_project();
    app.open_replace(Some("widget".into()));
    let panel = app.replace.as_ref().unwrap();
    assert_eq!(panel.find, "widget");
    // Three matching *lines* across two files, holding four matches.
    assert_eq!(hit_locs(&app), vec!["alpha.rs:0", "alpha.rs:2", "beta.rs:0"]);
    assert_eq!(panel.match_count(), 4, "the doubled line counts twice");
    assert_eq!(panel.summary(), "4 matches in 2 files");
}

#[test]
fn replace_panel_renders_inputs_results_and_a_before_after_preview() {
    let mut app = replace_project();
    app.open_replace(Some("widget".into()));
    typ(&mut app, ""); // focus starts on Find
    press(&mut app, KeyCode::Tab); // → Replace
    typ(&mut app, "gadget");
    let out = render(&app, 130, 44);
    contains_all(&out, &["Find", "Replace", "Matches", "widget", "gadget", "alpha.rs"]);
    // The preview shows the match line as a diff: what it is, and what it becomes.
    assert!(out.contains("- let widget = 1;"), "missing before line:\n{out}");
    assert!(out.contains("+ let gadget = 1;"), "missing after line:\n{out}");
}

#[test]
fn typing_in_find_re_searches_but_typing_in_replace_does_not() {
    let mut app = replace_project();
    app.open_replace(Some("widget".into()));
    assert_eq!(app.replace.as_ref().unwrap().hits.len(), 3);

    // Narrowing the pattern drops the non-matching hits.
    typ(&mut app, "s");
    assert!(app.replace.as_ref().unwrap().hits.is_empty(), "`widgets` matches nothing");
    press(&mut app, KeyCode::Backspace);
    assert_eq!(app.replace.as_ref().unwrap().hits.len(), 3);

    // The Replace field only changes the preview, never the result set.
    press(&mut app, KeyCode::Tab);
    typ(&mut app, "gadget");
    let panel = app.replace.as_ref().unwrap();
    assert_eq!(panel.hits.len(), 3);
    assert_eq!(panel.hits[0].preview, "let gadget = 1;");
}

#[test]
fn accepting_one_occurrence_edits_only_that_match() {
    let mut app = replace_project();
    app.open_replace(Some("widget".into()));
    press(&mut app, KeyCode::Tab);
    typ(&mut app, "gadget");
    press(&mut app, KeyCode::Tab); // → Results
    key(&mut app, 'j'); // the doubled line, alpha.rs:2
    key(&mut app, 'j'); // beta.rs — skip past, then come back
    key(&mut app, 'k');
    assert_eq!(app.replace.as_ref().unwrap().current().unwrap().line, 2);

    key(&mut app, 'y');

    let text = fs::read_to_string(app.root.join("alpha.rs")).unwrap();
    assert!(
        text.contains("println!(\"{gadget} {widget}\");"),
        "only the first match on the line changed:\n{text}"
    );
    // The list is re-derived, so the line still appears — with one match left.
    let panel = app.replace.as_ref().unwrap();
    assert_eq!(panel.match_count(), 3);
}

#[test]
fn accepting_all_rewrites_every_matched_file() {
    let mut app = replace_project();
    app.open_replace(Some("widget".into()));
    press(&mut app, KeyCode::Tab);
    typ(&mut app, "gadget");
    ctrl(&mut app, 'a'); // replace all, from the Replace field

    let alpha = fs::read_to_string(app.root.join("alpha.rs")).unwrap();
    let beta = fs::read_to_string(app.root.join("beta.rs")).unwrap();
    assert!(!alpha.contains("widget"), "alpha still has a match:\n{alpha}");
    assert!(alpha.contains("println!(\"{gadget} {gadget}\");"), "{alpha}");
    assert_eq!(beta.trim(), "use gadget::thing;");
    assert!(app.message.starts_with("4 replacements in 2 files"), "{}", app.message);
    // Re-searching after the rewrite finds nothing left.
    assert!(app.replace.as_ref().unwrap().hits.is_empty());
}

#[test]
fn an_edit_to_an_open_buffer_stays_unsaved_rather_than_hitting_disk() {
    let mut app = replace_project();
    let path = app.root.join("alpha.rs");
    app.open_path(path.clone(), "alpha.rs".into());
    assert!(app.is_file());

    app.open_replace(Some("widget".into()));
    press(&mut app, KeyCode::Tab);
    typ(&mut app, "gadget");
    ctrl(&mut app, 'a');

    // The open buffer holds the change and is marked modified...
    assert!(app.editor_lines().iter().any(|l| l.contains("gadget")), "{:?}", app.editor_lines());
    assert!(app.active_modified(), "the buffer should need a `:w`");
    // ...while the file on disk is untouched until it is written.
    let on_disk = fs::read_to_string(&path).unwrap();
    assert!(on_disk.contains("widget"), "disk was written behind the buffer:\n{on_disk}");
    // A file with no buffer is written straight through, though.
    assert!(!fs::read_to_string(app.root.join("beta.rs")).unwrap().contains("widget"));
}

#[test]
fn the_search_reads_unsaved_buffer_text_not_the_stale_file() {
    let mut app = replace_project();
    app.open_path(app.root.join("alpha.rs"), "alpha.rs".into());
    // Type a new occurrence into the buffer without saving.
    typ(&mut app, "Owidget again");
    press(&mut app, KeyCode::Esc);
    assert!(app.active_modified());

    app.open_replace(Some("widget".into()));
    assert_eq!(
        app.replace.as_ref().unwrap().match_count(),
        5,
        "the unsaved line's match is found too: {:?}",
        hit_locs(&app)
    );
}

#[test]
fn capture_groups_and_word_boundaries_work_as_in_substitute() {
    let mut app = temp_project(&[("a.rs", "call foo_old();\nlet fnord = foo_old;\n")]);
    app.open_replace(Some(r"\(\w\+\)_old".into()));
    press(&mut app, KeyCode::Tab);
    typ(&mut app, r"\1_new");
    ctrl(&mut app, 'a');
    let text = fs::read_to_string(app.root.join("a.rs")).unwrap();
    assert!(text.contains("call foo_new();"), "{text}");
    assert!(text.contains("let fnord = foo_new;"), "{text}");
}

#[test]
fn an_invalid_pattern_shows_an_error_instead_of_results() {
    let mut app = replace_project();
    app.open_replace(Some("widget".into()));
    assert_eq!(app.replace.as_ref().unwrap().hits.len(), 3);
    typ(&mut app, r"\("); // `widget\(` — an unclosed group
    let panel = app.replace.as_ref().unwrap();
    assert!(panel.error.is_some());
    assert!(panel.hits.is_empty(), "stale hits must not survive a broken pattern");
    let out = render(&app, 130, 44);
    assert!(out.contains("E486"), "the error should be on screen:\n{out}");
}

#[test]
fn ctrl_i_toggles_ignorecase_and_re_searches() {
    let mut app = temp_project(&[("a.rs", "Widget\n")]);
    app.open_replace(Some("widget".into()));
    assert!(app.replace.as_ref().unwrap().hits.is_empty());
    ctrl(&mut app, 'i');
    let panel = app.replace.as_ref().unwrap();
    assert!(panel.ignorecase);
    assert_eq!(panel.hits.len(), 1);
}

#[test]
fn enter_on_a_result_opens_the_file_at_the_match_and_closes_the_panel() {
    let mut app = replace_project();
    app.open_replace(Some("widget".into()));
    press(&mut app, KeyCode::Tab);
    press(&mut app, KeyCode::Tab); // → Results
    key(&mut app, 'j');
    key(&mut app, 'j'); // beta.rs:0
    press(&mut app, KeyCode::Enter);

    assert!(app.replace.is_none(), "jumping closes the panel");
    assert_eq!(app.active_buffer().label, "beta.rs");
    assert_eq!(app.editor_cursor(), (0, 4), "cursor on the match, not the line start");
}

#[test]
fn esc_closes_the_panel_without_changing_anything() {
    let mut app = replace_project();
    app.open_replace(Some("widget".into()));
    press(&mut app, KeyCode::Tab);
    typ(&mut app, "gadget");
    press(&mut app, KeyCode::Esc);
    assert!(app.replace.is_none());
    assert!(fs::read_to_string(app.root.join("alpha.rs")).unwrap().contains("widget"));
}

#[test]
fn the_find_command_opens_the_panel_seeded_from_the_cursor() {
    let mut app = replace_project();
    app.open_path(app.root.join("alpha.rs"), "alpha.rs".into());
    typ(&mut app, "wl"); // cursor onto `widget` (past `let `)
    app.run_ex_command("Find");
    assert_eq!(app.replace.as_ref().unwrap().find, "widget");

    // And an explicit argument wins over the cursor.
    app.dispatch(Action::CloseReplace);
    app.run_ex_command("Find other");
    assert_eq!(app.replace.as_ref().unwrap().find, "other");
}

#[test]
fn tab_cycles_the_three_fields_and_shift_tab_goes_back() {
    use ctrlvim::replace::Field;
    let mut app = replace_project();
    app.open_replace(Some("widget".into()));
    let focus = |a: &App| a.replace.as_ref().unwrap().focus;
    assert_eq!(focus(&app), Field::Find);
    press(&mut app, KeyCode::Tab);
    assert_eq!(focus(&app), Field::Replace);
    press(&mut app, KeyCode::Tab);
    assert_eq!(focus(&app), Field::Results);
    press(&mut app, KeyCode::Tab);
    assert_eq!(focus(&app), Field::Find);
    press(&mut app, KeyCode::BackTab);
    assert_eq!(focus(&app), Field::Results, "shift-tab walks back");
}

#[test]
fn the_panel_survives_a_tiny_terminal() {
    let mut app = replace_project();
    app.open_replace(Some("widget".into()));
    for (w, h) in [(30u16, 10u16), (60, 14), (80, 20), (200, 60)] {
        let _ = render(&app, w, h);
    }
}
#[test]
fn leader_s_opens_the_panel_from_the_editor_and_the_dashboard() {
    // From a file buffer: <Space>S goes through the engine keymap.
    let mut app = replace_project();
    app.open_path(app.root.join("alpha.rs"), "alpha.rs".into());
    key(&mut app, ' ');
    input::handle_key(&mut app, KeyEvent::new(KeyCode::Char('S'), KeyModifiers::SHIFT));
    assert!(app.replace.is_some(), "leader-S in the editor");

    // From the dashboard: the shell's own leader handling.
    let mut app = replace_project();
    key(&mut app, ' ');
    input::handle_key(&mut app, KeyEvent::new(KeyCode::Char('S'), KeyModifiers::SHIFT));
    assert!(app.replace.is_some(), "leader-S on the dashboard");
}

#[test]
fn the_panel_floats_over_the_screen_rather_than_replacing_it() {
    let mut app = replace_project();
    app.open_replace(Some("widget".into()));
    let out = render(&app, 130, 44);

    // The dashboard behind it is still visible on every side — that is what
    // makes this read as a popup instead of a screen you navigated to.
    contains_all(&out, &["ctrlvim", "workspace", "settings", "about"]);
    // Every row has screen content outside the popup's left and right edges.
    let framed = out
        .lines()
        .filter(|l| l.contains("\u{256d}") || l.contains("\u{2570}"))
        .count();
    assert!(framed > 0, "expected rounded popup borders:\n{out}");
    // Top and bottom rows belong to the shell, not the panel.
    let lines: Vec<&str> = out.lines().collect();
    assert!(lines[0].contains("Dashboard"), "tab bar survives: {:?}", lines[0]);
    assert!(
        lines.iter().rev().take(2).any(|l| l.contains("NORMAL")),
        "status line survives:\n{out}"
    );
}

#[test]
fn the_panel_wears_the_same_chrome_as_the_file_browser() {
    // Both are centered popups with rounded boxes and centered border titles;
    // drifting apart is a regression in how coherent the editor feels.
    let mut app = replace_project();
    app.dispatch(Action::OpenFinder);
    let browser = render(&app, 130, 44);
    app.dispatch(Action::CloseFinder);
    app.open_replace(Some("widget".into()));
    let panel = render(&app, 130, 44);

    for out in [&browser, &panel] {
        assert!(out.contains('\u{256d}'), "rounded top-left corner:\n{out}");
        assert!(out.contains('\u{256f}'), "rounded bottom-right corner:\n{out}");
    }
    // The panel's boxes are titled and centered like the browser's.
    contains_all(&panel, &["Find", "Replace", "Matches"]);
}

#[test]
fn a_narrow_terminal_drops_the_preview_rather_than_squeezing_it() {
    let mut app = replace_project();
    app.open_replace(Some("widget".into()));
    // Wide: both columns.
    let wide = render(&app, 130, 44);
    assert!(wide.contains("- let widget = 1;"), "preview column expected:\n{wide}");
    // Narrow: the results list takes the whole popup, no preview.
    let narrow = render(&app, 62, 30);
    assert!(!narrow.contains("- let widget = 1;"), "preview should be dropped:\n{narrow}");
    assert!(narrow.contains("alpha.rs"), "results still shown:\n{narrow}");
}

// --- inline AI suggestions --------------------------------------------------
//
// The model itself is never loaded here: a suggestion is delivered through the
// engine's ordinary request/reply path, which is exactly what the completion
// worker does when it finishes. That keeps these tests about the *editor* —
// ghost text, accepting, dismissing, the status badge — rather than about
// candle.

/// A file buffer in Insert mode with ghost text showing.
fn app_with_ghost(source: &str, keys: &str, ghost: &str) -> App {
    let mut app = temp_project(&[("main.rs", source)]);
    app.open_path(app.root.join("main.rs"), "main.rs".into());
    app.engine.session.set_suggestions_enabled(true);
    for c in keys.chars() {
        key(&mut app, c);
    }
    let req = app
        .engine
        .session
        .suggest_request()
        .expect("insert mode wants a completion");
    assert!(
        app.engine.session.fulfill_suggestion(req.seq, ghost),
        "the reply should have been shown"
    );
    app
}

#[test]
fn ghost_text_is_drawn_at_the_cursor_but_is_not_in_the_buffer() {
    let mut app = app_with_ghost("fn main() {\n\n}\n", "ji", "    println!(\"hi\");");
    let out = render(&app, 100, 20);
    assert!(out.contains("println!(\"hi\");"), "ghost text expected:\n{out}");
    // Proposed, not present: the buffer is untouched until it is accepted.
    assert_eq!(app.editor_lines()[1], "");

    // Dismissing takes it back off the screen.
    ctrl(&mut app, 'e');
    let after = render(&app, 100, 20);
    assert!(!after.contains("println!"), "dismissed ghost should be gone:\n{after}");
}

#[test]
fn ghost_text_is_dimmer_than_real_code() {
    // The whole point is that it reads as a proposal at a glance.
    let app = app_with_ghost("fn main() {\n\n}\n", "ji", "todo");
    let buf = render_cells(&app, 100, 20);
    assert_ne!(fg_of(&buf, "todo", 0), fg_of(&buf, "fn main", 0));
}

#[test]
fn a_suggestion_offered_mid_line_pushes_the_existing_text_right() {
    // The regression this guards: ghost text painted over the rest of the line
    // used to hide the `)` the cursor was sitting in front of.
    let app = app_with_ghost("let x = f();\n", "i", "value");
    let out = render(&app, 100, 20);
    assert!(out.contains("valuelet x = f();"), "line tail expected after ghost:\n{out}");
}

#[test]
fn tab_accepts_ghost_text_into_the_buffer() {
    let mut app = app_with_ghost("fn main() {\n\n}\n", "ji", "    body();");
    press(&mut app, KeyCode::Tab);
    assert_eq!(app.editor_lines()[1], "    body();");
    assert!(app.suggestion().is_none());
    // …and it survives into what would be written out.
    assert!(render(&app, 100, 20).contains("body();"));
}

#[test]
fn a_multiline_suggestion_shows_its_continuation_rows() {
    let app = app_with_ghost("fn main() {\n\n}\n", "ji", "    if x {\n        y();\n    }");
    let out = render(&app, 100, 20);
    contains_all(&out, &["if x {", "y();"]);
}

#[test]
fn the_status_line_shows_nothing_until_suggestions_are_switched_on() {
    let app = temp_project(&[("main.rs", "fn main() {}\n")]);
    assert!(app.ai_badge().is_none());
    assert!(!render(&app, 130, 44).contains("AI"));
}

#[test]
fn the_ai_command_toggles_suggestions_and_says_so() {
    let mut app = temp_project(&[("main.rs", "fn main() {}\n")]);
    app.open_path(app.root.join("main.rs"), "main.rs".into());

    app.run_ex_command("AI on");
    assert!(app.engine.session.suggest.enabled);
    assert!(app.message.contains("on"), "got {:?}", app.message);
    // Enabling starts the worker but must not load anything on its own — a
    // multi-gigabyte download is not a side effect of typing `:AI on`.
    assert_eq!(app.ai_status(), Some(ctrlvim_ai::Status::Cold));
    assert_eq!(app.ai_badge(), Some("AI"));

    app.run_ex_command("AI off");
    assert!(!app.engine.session.suggest.enabled);
    assert!(app.ai_status().is_none(), "the worker is dropped with the feature");

    // A bare `:AI` flips whatever the current state is.
    app.run_ex_command("AI");
    assert!(app.engine.session.suggest.enabled);
}

#[test]
fn ai_status_reports_the_model_state_without_loading_it() {
    let mut app = temp_project(&[("main.rs", "fn main() {}\n")]);
    app.run_ex_command("AIStatus");
    assert!(app.message.contains("off"), "got {:?}", app.message);
    app.run_ex_command("AI on");
    app.run_ex_command("AIStatus");
    assert!(app.message.contains("not loaded"), "got {:?}", app.message);
}

#[test]
fn the_poll_interval_tightens_only_while_suggestions_are_armed() {
    // A 250ms loop would deliver every completion a quarter second late, on top
    // of a model that is already the slow part.
    let mut app = temp_project(&[("main.rs", "fn main() {}\n")]);
    let idle = app.poll_interval();
    app.run_ex_command("AI on");
    assert!(app.poll_interval() < idle, "armed loop should poll faster");
    app.run_ex_command("AI off");
    assert_eq!(app.poll_interval(), idle);
}

#[test]
fn the_settings_tab_has_a_row_for_inline_ai_suggestions() {
    // Deliberately only *rendered*, never toggled: `toggle_ai` persists to the
    // real `~/.config/ctrlvim/config.toml`. That half is covered by
    // `config::tests::toggling_ai_persists_without_flattening_the_rest`, which
    // writes to a temp path.
    let mut app = temp_project(&[("main.rs", "x\n")]);
    app.dispatch(Action::GotoSection(DashboardSection::Settings));
    let out = render(&app, 130, 44);
    contains_all(&out, &["EDITOR", "Inline AI suggestions", "off"]);
    assert!(!app.ai_enabled(), "off until switched on");
}

#[test]
fn the_settings_row_follows_the_live_state_not_just_the_config_file() {
    // `:AI on` and the checkbox must never disagree about whether suggestions
    // are running.
    let mut app = temp_project(&[("main.rs", "x\n")]);
    app.dispatch(Action::GotoSection(DashboardSection::Settings));
    app.run_ex_command("AI on");
    assert!(app.ai_enabled());
    let out = render(&app, 130, 44);
    let row = out
        .lines()
        .find(|l| l.contains("Inline AI suggestions"))
        .expect("the AI row renders");
    assert!(row.contains("on"), "row should read as on: {row:?}");
}


#[test]
fn rendering_a_markdown_buffer_that_ends_in_a_blank_line_does_not_panic() {
    // The crash this guards: a `Buffer`'s rope always ends in `\n`, so
    // `editor_lines()` reports a final empty line, while `ctrlvim_markdown`
    // deliberately drops that phantom line. The decorated vector was therefore
    // one short of the buffer, and drawing the last row indexed past its end —
    // `index out of bounds: the len is N but the index is N`.
    //
    // A freshly-opened file doesn't show it (`open_path` splits with `.lines()`,
    // which strips the trailing newline); *any edit that leaves a blank last
    // line* does, and markdown files render live by default, so this was one
    // `o` away on every `.md` buffer in the project.
    let src: String = (1..=30).map(|i| format!("# line {i}\n")).collect();
    let mut app = temp_project(&[("doc.md", &src)]);
    app.open_path(app.root.join("doc.md"), "doc.md".into());
    assert!(app.md_render_active(), "markdown buffers render live by default");
    assert_eq!(app.editor_lines().len(), 30);

    typ(&mut app, "Go"); // jump to the last line, open a blank one below it
    press(&mut app, KeyCode::Esc);
    assert_eq!(app.editor_lines().len(), 31, "the buffer now ends in a blank line");

    // Tall enough that the blank last line is on screen — that is the row that
    // used to panic.
    let out = render(&app, 100, 40);
    assert!(out.contains("line 30"), "the document still renders:\n{out}");

    // …and with rendering off, which takes the other arm of the same match.
    ctrl(&mut app, 'g');
    assert!(!app.md_render_active());
    render(&app, 100, 40);
}

#[test]
fn a_gated_model_explains_itself_in_a_panel_not_a_clipped_status_line() {
    // The bug this guards: the gated-repo error is the single most likely
    // failure of this feature, and the whole point of it is the instructions.
    // As one long line it got cut off at the terminal edge, leaving the user
    // with "AI: google/codegemma-2b is gated" and nowhere to go.
    let mut app = temp_project(&[("main.rs", "fn main() {}\n")]);
    let error = ctrlvim_ai::gated_repo_help("google/codegemma-2b");
    app.show_ai_error(&error);

    // Status line gets a self-contained summary…
    assert!(app.message.starts_with("AI: "), "got {:?}", app.message);
    assert!(!app.message.contains('\n'), "one line only: {:?}", app.message);

    // …and the panel gets everything that matters.
    assert!(app.shell_open, "the panel opens");
    let out = render(&app, 100, 30);
    contains_all(
        &out,
        &[
            "huggingface.co/google/codegemma-2b", // where to accept the license
            "HF_TOKEN",                           // how to authenticate
            "AILoad",                             // how to retry
            "unsloth/codegemma-2b",               // the ungated way out
            "docs/ai.md",
        ],
    );

    // Dismissible like any other output panel.
    press(&mut app, KeyCode::Esc);
    assert!(!app.shell_open);
}

#[test]
fn a_one_line_ai_error_stays_on_the_status_line() {
    // Not every failure deserves a modal; only ones with something to teach.
    let mut app = temp_project(&[("main.rs", "fn main() {}\n")]);
    app.show_ai_error("tokenizer: unexpected end of input");
    assert!(app.message.contains("tokenizer"));
    assert!(!app.shell_open, "a short error needs no panel");
}

/// Write a throwaway config file and return its path.
fn temp_config(body: &str) -> std::path::PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let path = std::env::temp_dir()
        .join(format!("ctrlvim-reload-{}-{}.toml", std::process::id(), n));
    fs::write(&path, body).unwrap();
    path
}

#[test]
fn ai_load_rereads_the_config_so_a_fixed_repo_actually_takes_effect() {
    // `gated_repo_help` tells the user to edit `[ai.model] repo` and then run
    // `:AILoad`. That advice is only true if the reload re-reads the file —
    // otherwise the retry uses the repo the editor booted with and fails
    // identically, which looks like a broken command.
    let mut app = temp_project(&[("main.rs", "fn main() {}\n")]);
    app.run_ex_command("AI on");
    // Whatever the shipped default happens to be — which repo that is belongs
    // to `the_default_model_is_quantized_and_ungated`, not here.
    let booted = app.config.ai.model.repo.clone();
    assert!(app.ai_status().is_some(), "a worker exists for the booted config");

    let path = temp_config("[ai]\nenabled = true\n\n[ai.model]\nrepo = \"some-mirror/codegemma-2b\"\n");
    assert!(app.reload_ai_config_from(&path), "the section changed");
    assert!(
        app.ai_status().is_none(),
        "the worker built from the old repo was dropped, so the next load uses the new one"
    );
    let _ = fs::remove_file(&path);
}

#[test]
fn reloading_an_unchanged_config_keeps_the_loaded_worker() {
    // Re-reading must not throw away several gigabytes of resident weights
    // just because someone ran `:AILoad` twice.
    let mut app = temp_project(&[("main.rs", "fn main() {}\n")]);
    app.run_ex_command("AI on");
    // A file whose `[ai]` resolves to exactly what the app already holds.
    // (`:AI on` is session state; it deliberately doesn't touch `config.ai`.)
    let path = temp_config("# no [ai] section, so every knob is the default\n");
    assert!(!app.reload_ai_config_from(&path), "nothing differs from what's loaded");
    assert!(app.ai_status().is_some(), "the worker survives");
    let _ = fs::remove_file(&path);
}

#[test]
fn reloading_picks_up_every_ai_knob_not_just_the_repo() {
    let mut app = temp_project(&[("main.rs", "fn main() {}\n")]);
    app.run_ex_command("AI on");
    let path = temp_config(
        "[ai]\nenabled = true\ndevice = \"cpu\"\nmax_tokens = 16\ncontext_before = 5\n",
    );
    assert!(app.reload_ai_config_from(&path));
    assert_eq!(app.config.ai.device, ctrlvim_ai::DevicePref::Cpu);
    assert_eq!(app.config.ai.max_tokens, 16);
    // The context window lives in the engine, so it has to be pushed across.
    assert_eq!(app.engine.session.suggest.context.before, 5);
    let _ = fs::remove_file(&path);
}
