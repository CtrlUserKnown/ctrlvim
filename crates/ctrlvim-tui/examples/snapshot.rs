//! Prints a plain-text snapshot of a chosen screen to stdout — a quick way to
//! eyeball layout without a live terminal. Usage:
//!   cargo run -p ctrlvim-tui --example snapshot -- [dashboard|settings|about|plugins|drawer|finder|palette|help|file]

use ctrlvim_tui::app::{Action, App, DashboardSection};
use ctrlvim_tui::ui;
use ratatui::backend::TestBackend;
use ratatui::Terminal;

fn main() {
    let which = std::env::args().nth(1).unwrap_or_else(|| "dashboard".into());
    let mut app = App::new();
    match which.as_str() {
        "settings" => app.dispatch(Action::GotoSection(DashboardSection::Settings)),
        "about" => app.dispatch(Action::GotoSection(DashboardSection::About)),
        "plugins" => app.dispatch(Action::OpenPlugins),
        "drawer" => app.dispatch(Action::ToggleSidebar),
        "finder" => app.dispatch(Action::OpenFinder),
        "palette" => app.dispatch(Action::OpenPalette),
        "help" => app.dispatch(Action::ToggleHelp),
        "file" => app.open_file(0),
        _ => {}
    }

    let (w, h) = (110u16, 34u16);
    let backend = TestBackend::new(w, h);
    let mut term = Terminal::new(backend).unwrap();
    term.draw(|f| {
        ui::draw(f, &app);
    })
    .unwrap();

    let buf = term.backend().buffer().clone();
    println!("┌{}┐", "─".repeat(w as usize));
    for y in 0..buf.area.height {
        let mut row = String::new();
        for x in 0..buf.area.width {
            row.push_str(buf[(x, y)].symbol());
        }
        println!("│{row}│");
    }
    println!("└{}┘", "─".repeat(w as usize));
}
