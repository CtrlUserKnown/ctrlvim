//! ctrlvim's Ratatui frontend: the startup dashboard and editor shell that sit
//! on top of the ctrlvim engine (`ctrlvim-core`).
//!
//! Run with `cargo run -p ctrlvim-tui`. Keyboard and mouse both drive the same
//! actions; see the `?` help overlay for the keymap.

use std::io::{self, Stdout};
use std::panic;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind, MouseButton, MouseEventKind,
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

fn main() -> io::Result<()> {
    let start = Instant::now();
    install_panic_hook();
    let mut terminal = setup_terminal()?;
    let res = run(&mut terminal, start);
    restore_terminal(&mut terminal)?;
    res
}

fn run(terminal: &mut Term, start: Instant) -> io::Result<()> {
    // Reflect the project in the current working directory.
    let root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut app = App::with_root(root, start);
    let mut zones = Zones::default();

    while !app.should_quit {
        terminal.draw(|f| {
            zones = ui::draw(f, &app);
        })?;

        // Block for the next event (with a poll so resize repaints promptly).
        if !event::poll(Duration::from_millis(250))? {
            continue;
        }
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
            _ => {}
        }
    }
    Ok(())
}

fn setup_terminal() -> io::Result<Term> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    Terminal::new(CrosstermBackend::new(stdout))
}

fn restore_terminal(terminal: &mut Term) -> io::Result<()> {
    disable_raw_mode()?;
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
        let _ = execute!(stdout, LeaveAlternateScreen, DisableMouseCapture);
        original(info);
    }));
}
