//! ctrlvim's Ratatui frontend: the startup dashboard and editor shell that sit
//! on top of the ctrlvim engine (`ctrlvim-core`).
//!
//! Run with `cargo run -p ctrlvim`, or `cvi` once installed. Keyboard and mouse
//! both drive the same actions; see the `?` help overlay for the keymap.
//!
//! The command line is deliberately small. ctrlvim is dashboard-first: it
//! reflects a *project* rather than a file, so the interesting argument is the
//! directory, and `cvi` with no arguments at all is the normal way to start it.
//! Paths are accepted because a binary on `$PATH` that ignores `cvi file.rs` is
//! a surprise no amount of documentation fixes.

use std::io::{self, Stdout};
use std::panic;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind, KeyboardEnhancementFlags,
    MouseButton, MouseEventKind, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use ctrlvim::app::App;
use ctrlvim::input;
use ctrlvim::ui::{self, Zones};

type Term = Terminal<CrosstermBackend<Stdout>>;

const USAGE: &str = "\
cvi — ctrlvim, a modal editor

USAGE:
    cvi [OPTIONS] [PATH]...

ARGS:
    <PATH>...    Files to open. A directory sets the project root that the
                 dashboard and finders reflect; with no directory given, the
                 current working directory is used.

OPTIONS:
    -h, --help       Print this help and exit
    -V, --version    Print version and exit

Press `?` inside the editor for the keymap.
";

fn main() -> io::Result<()> {
    let start = Instant::now();
    // Arguments are handled before the terminal is touched: `--version` has to
    // print to stdout, not into an alternate screen that is then torn down.
    let launch = match parse_args(std::env::args().skip(1), |p| p.is_dir()) {
        Ok(Invocation::Print(text)) => {
            print!("{text}");
            return Ok(());
        }
        Ok(Invocation::Launch(l)) => l,
        Err(msg) => {
            eprintln!("cvi: {msg}");
            eprint!("{USAGE}");
            std::process::exit(2);
        }
    };
    install_panic_hook();
    let mut terminal = setup_terminal()?;
    let res = run(&mut terminal, start, launch);
    restore_terminal(&mut terminal)?;
    res
}

/// What the command line asked for.
#[derive(Debug, PartialEq, Eq)]
enum Invocation {
    /// Print this text and exit successfully (`--help`, `--version`).
    Print(String),
    Launch(Launch),
}

/// How the editor should start.
#[derive(Debug, Default, PartialEq, Eq)]
struct Launch {
    /// Project root. `None` means the current working directory.
    root: Option<PathBuf>,
    /// Files to open, in the order given.
    files: Vec<PathBuf>,
}

/// Parse the command line.
///
/// `is_dir` is injected rather than called directly so the classification rule
/// can be tested without touching the filesystem. A path that exists and is a
/// directory becomes the project root; everything else is a file to open —
/// including a path that does not exist yet, which opens an empty buffer under
/// that name exactly as `vim newfile.txt` does.
fn parse_args<I>(args: I, is_dir: impl Fn(&Path) -> bool) -> Result<Invocation, String>
where
    I: IntoIterator<Item = String>,
{
    let mut launch = Launch::default();
    let mut only_paths = false;
    for arg in args {
        // Everything after `--` is a path, so a file really named `-h` is
        // still reachable.
        if !only_paths && arg == "--" {
            only_paths = true;
            continue;
        }
        if !only_paths && arg.len() > 1 && arg.starts_with('-') {
            match arg.as_str() {
                "-h" | "--help" => return Ok(Invocation::Print(USAGE.to_string())),
                "-V" | "--version" => {
                    return Ok(Invocation::Print(format!(
                        "cvi {} — ctrlvim\n",
                        env!("CARGO_PKG_VERSION")
                    )))
                }
                _ => return Err(format!("unknown option '{arg}'")),
            }
        }
        let path = PathBuf::from(arg);
        if is_dir(&path) {
            if launch.root.is_some() {
                return Err("more than one directory given".to_string());
            }
            launch.root = Some(path);
        } else {
            launch.files.push(path);
        }
    }
    Ok(Invocation::Launch(launch))
}

/// The name a buffer carries for a path — the file name alone, since the tab
/// bar has no room for a full path.
fn buffer_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

fn run(terminal: &mut Term, start: Instant, launch: Launch) -> io::Result<()> {
    // Reflect the project in the directory given, or the working directory.
    let root = launch
        .root
        .map(|r| r.canonicalize().unwrap_or(r))
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let mut app = App::with_root(root, start);
    // Apply config.toml (options, mappings, plugins) and fire `VimEnter` before
    // the first frame, so startup state is what the user configured.
    app.apply_config();
    // Files open after the config so options, mappings and the Lua host are all
    // in place before `BufEnter` fires for them.
    for path in &launch.files {
        let abs = path.canonicalize().unwrap_or_else(|_| path.clone());
        app.open_path(abs, buffer_name(path));
    }
    // Vim leaves the cursor in the *first* file, not the last. Re-opening
    // focuses an existing buffer rather than duplicating it, so this just moves
    // focus back once they are all loaded.
    if let Some(first) = launch.files.first() {
        let abs = first.canonicalize().unwrap_or_else(|_| first.clone());
        app.open_path(abs, buffer_name(first));
    }
    let mut zones = Zones::default();

    while !app.should_quit {
        terminal.draw(|f| {
            zones = ui::draw(f, &app);
        })?;

        // Block for the next event (with a poll so resize repaints promptly).
        // The timeout is also where background job output and finished AI
        // completions are picked up, so a long `:make` streams into the
        // quickfix list — and ghost text appears — without blocking keys. How
        // long to wait is the app's call: inline suggestions need a tighter
        // loop than a build does (see `App::poll_interval`).
        // ...and where a half-typed mapping's `'timeoutlen'` runs out, which
        // is what lets `<leader>q` and `<leader>qq` coexist.
        if !event::poll(app.poll_interval())? {
            app.poll_jobs();
            app.poll_ai();
            app.tick_keymap_timeout();
            continue;
        }
        app.poll_jobs();
        app.poll_ai();
        app.tick_keymap_timeout();
        match event::read()? {
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                input::handle_key(&mut app, key);
            }
            Event::Mouse(m) if m.kind == MouseEventKind::Down(MouseButton::Left) => {
                if let Some(action) = zones.hit(m.column, m.row) {
                    app.dispatch(action.clone());
                }
            }
            // Mouse-wheel scrolling in the editor (opt-in via `mouse` config).
            Event::Mouse(m) if m.kind == MouseEventKind::ScrollDown => app.scroll_editor(3),
            Event::Mouse(m) if m.kind == MouseEventKind::ScrollUp => app.scroll_editor(-3),
            // A resize can leave stale glyphs behind: ratatui only repaints
            // cells that changed since its last known buffer, and a terminal
            // that grew (or whose emulator redraws lazily mid-resize) can
            // leave old content in the newly-exposed area. `clear()` blanks
            // the real screen and forces a full repaint on the next `draw`.
            Event::Resize(_, _) => terminal.clear()?,
            _ => {}
        }
    }
    Ok(())
}

fn setup_terminal() -> io::Result<Term> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    enable_key_disambiguation(&mut stdout);
    Terminal::new(CrosstermBackend::new(stdout))
}

/// Ask for the kitty keyboard protocol, which is what makes `<C-S-j>`
/// distinguishable from `<C-j>` (and `<C-Up>` from `<Up>`).
///
/// A legacy terminal encodes both of those the same way, so without this the
/// shifted mappings simply never fire — there is nothing in the byte stream to
/// match on. Support is not universal, so this is best-effort: the request is
/// ignored by terminals that don't implement it, and a failure to write it is
/// not worth refusing to start over.
fn enable_key_disambiguation(out: &mut impl io::Write) {
    let _ = execute!(
        out,
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
    );
}

/// Undo [`enable_key_disambiguation`]. Leaving the flags pushed would hand the
/// user's shell a terminal that reports keys in a mode it doesn't expect.
fn disable_key_disambiguation(out: &mut impl io::Write) {
    let _ = execute!(out, PopKeyboardEnhancementFlags);
}

fn restore_terminal(terminal: &mut Term) -> io::Result<()> {
    disable_raw_mode()?;
    disable_key_disambiguation(terminal.backend_mut());
    execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture)?;
    terminal.show_cursor()?;
    Ok(())
}

/// Restore the terminal on panic so a crash doesn't leave it in raw mode.
fn install_panic_hook() {
    let original = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        let mut stdout = io::stdout();
        let _ = disable_raw_mode();
        disable_key_disambiguation(&mut stdout);
        let _ = execute!(stdout, LeaveAlternateScreen, DisableMouseCapture);
        original(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Classify by a trailing slash, so the tests never touch the filesystem.
    fn fake_dir(p: &Path) -> bool {
        p.to_string_lossy().ends_with('/')
    }

    fn parse(args: &[&str]) -> Result<Invocation, String> {
        parse_args(args.iter().map(|s| s.to_string()), fake_dir)
    }

    fn launch(args: &[&str]) -> Launch {
        match parse(args).expect("parses") {
            Invocation::Launch(l) => l,
            Invocation::Print(t) => panic!("expected a launch, got output: {t}"),
        }
    }

    #[test]
    fn no_arguments_launches_in_the_working_directory() {
        assert_eq!(launch(&[]), Launch::default());
    }

    #[test]
    fn help_and_version_print_instead_of_launching() {
        let Ok(Invocation::Print(help)) = parse(&["--help"]) else { panic!("expected output") };
        assert!(help.contains("USAGE:"));
        assert_eq!(parse(&["-h"]), parse(&["--help"]));

        let Ok(Invocation::Print(v)) = parse(&["--version"]) else { panic!("expected output") };
        assert!(v.starts_with("cvi "), "got {v:?}");
        assert!(v.contains(env!("CARGO_PKG_VERSION")));
        assert_eq!(parse(&["-V"]), parse(&["--version"]));
    }

    #[test]
    fn a_directory_becomes_the_root_and_files_become_buffers() {
        let l = launch(&["src/", "a.rs", "b.rs"]);
        assert_eq!(l.root, Some(PathBuf::from("src/")));
        assert_eq!(l.files, vec![PathBuf::from("a.rs"), PathBuf::from("b.rs")]);
    }

    #[test]
    fn order_does_not_matter_for_the_directory() {
        assert_eq!(launch(&["a.rs", "src/"]), launch(&["src/", "a.rs"]));
    }

    #[test]
    fn a_path_that_does_not_exist_is_a_file_to_create() {
        // `vim newfile.txt` opens an empty buffer rather than failing, and so
        // does this: anything not a directory is a file.
        let l = launch(&["newfile.txt"]);
        assert_eq!(l.files, vec![PathBuf::from("newfile.txt")]);
        assert!(l.root.is_none());
    }

    #[test]
    fn two_directories_are_an_error_rather_than_a_silent_choice() {
        assert!(parse(&["src/", "docs/"]).is_err());
    }

    #[test]
    fn an_unknown_option_is_rejected() {
        let err = parse(&["--nope"]).expect_err("should be rejected");
        assert!(err.contains("--nope"), "got {err:?}");
    }

    #[test]
    fn a_double_dash_makes_everything_after_it_a_path() {
        let l = launch(&["--", "-h", "--nope"]);
        assert_eq!(l.files, vec![PathBuf::from("-h"), PathBuf::from("--nope")]);
    }

    #[test]
    fn a_buffer_is_named_by_its_file_not_its_path() {
        assert_eq!(buffer_name(Path::new("crates/ctrlvim-tui/src/main.rs")), "main.rs");
        assert_eq!(buffer_name(Path::new("main.rs")), "main.rs");
    }
}
