//! Floating overlays drawn on top of the shell: the file explorer drawer, the
//! command palette, and the keybinding help modal. Each registers a
//! full-screen "scrim" zone that closes it on click-outside, then draws its
//! panel (whose own zones are registered afterwards, so they win hit-testing).

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::app::{Action, App};
use crate::theme;

use super::{centered, icon_chip, row_style, selection_bar, Zones};

/// Left-anchored file explorer drawer (Ctrl+B).
pub fn explorer(f: &mut Frame, app: &App, area: Rect, zones: &mut Zones) {
    zones.push(area, Action::CloseSidebar); // click-outside closes

    let w = 34u16.min(area.width);
    let panel = Rect { x: area.x, y: area.y, width: w, height: area.height };
    f.render_widget(Clear, panel);
    let block = Block::default()
        .borders(Borders::RIGHT)
        .border_style(Style::default().fg(theme::BORDER))
        .style(Style::default().bg(theme::BG_DARK));
    let inner = block.inner(panel);
    f.render_widget(block, panel);
    // Swallow clicks on the panel itself so they don't fall through to close.
    zones.push(panel, Action::None);

    if inner.height == 0 || inner.width == 0 {
        return;
    }
    let x = inner.x + 1;
    let text_w = inner.width.saturating_sub(2);
    let bottom = inner.y + inner.height;
    let mut y = inner.y;
    // Render a single line only if it fits within the drawer.
    macro_rules! line_at {
        ($yy:expr, $line:expr) => {
            if $yy < bottom {
                f.render_widget(Paragraph::new($line), Rect { x, y: $yy, width: text_w, height: 1 });
            }
        };
    }

    // Header: EXPLORER   ×
    line_at!(y, Line::from(Span::styled("EXPLORER", Style::default().fg(theme::FG_DIM).add_modifier(Modifier::BOLD))));
    let close_rect = Rect { x: inner.x + inner.width.saturating_sub(2), y, width: 1, height: 1 };
    if y < bottom {
        f.render_widget(Paragraph::new(Span::styled("×", Style::default().fg(theme::FG_DIM))), close_rect);
        zones.push(close_rect, Action::CloseSidebar);
    }
    y += 2;

    // Project root (the real cwd directory name).
    let root_name = app
        .root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| app.root.display().to_string());
    line_at!(y, Line::from(Span::styled(format!("▾ {root_name}"), Style::default().fg(theme::BLUE).add_modifier(Modifier::BOLD))));
    y += 1;

    // File list.
    for (i, file) in app.project.recent_files.iter().enumerate() {
        if y >= bottom {
            break;
        }
        let selected = i == app.file_index;
        let row = Rect { x: inner.x, y, width: inner.width, height: 1 };
        f.render_widget(Block::default().style(row_style(selected)), row);
        let spans = vec![
            selection_bar(selected, theme::BLUE),
            Span::raw(" "),
            icon_chip(file.icon_letter, file.icon_color),
            Span::raw(" "),
            Span::styled(file.name.clone(), Style::default().fg(if selected { theme::FG } else { theme::FG_MUTED })),
        ];
        f.render_widget(Paragraph::new(Line::from(spans)).style(row_style(selected)), row);
        zones.push(row, Action::OpenFile(i));
        y += 1;
    }

    y += 1;
    line_at!(y, Line::from(Span::styled("GIT", Style::default().fg(theme::FG_DIM).add_modifier(Modifier::BOLD))));
    y += 1;
    if let Some(g) = &app.project.git {
        line_at!(y, Line::from(Span::styled(format!("  {}", g.branch), Style::default().fg(theme::PURPLE))));
        y += 1;
        line_at!(
            y,
            Line::from(vec![
                Span::styled(format!("↑{}", g.ahead), Style::default().fg(theme::GREEN)),
                Span::styled(" · ", Style::default().fg(theme::FG_DIM)),
                Span::styled(format!("~{}", g.modified), Style::default().fg(theme::ORANGE)),
                Span::styled(" · ", Style::default().fg(theme::FG_DIM)),
                Span::styled(format!("+{}", g.staged), Style::default().fg(theme::CYAN)),
            ])
        );
    } else {
        line_at!(y, Line::from(Span::styled("not a repo", Style::default().fg(theme::FG_DIM))));
    }
}

/// Centered-near-top command palette (`:`).
pub fn palette(f: &mut Frame, app: &App, area: Rect, zones: &mut Zones) {
    zones.push(area, Action::ClosePalette);

    // Too small to draw the palette meaningfully.
    if area.width < 12 || area.height < 6 {
        return;
    }

    let results = app.palette_results();
    let w = 62u16.min(area.width.saturating_sub(4));
    let top = area.y + 3;
    let avail = (area.y + area.height).saturating_sub(top);
    let list_h = (results.len() as u16).min(10).min(avail.saturating_sub(4));
    let h = (list_h + 4).min(avail);
    let panel = Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: top,
        width: w,
        height: h,
    };
    f.render_widget(Clear, panel);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme::BORDER))
        .style(Style::default().bg(theme::BG_DARK));
    let inner = block.inner(panel);
    f.render_widget(block, panel);

    // Input row: ": query".
    let query = if app.palette_query.is_empty() {
        Span::styled("type a command or file...", Style::default().fg(theme::FG_DIM))
    } else {
        Span::styled(app.palette_query.clone(), Style::default().fg(theme::FG))
    };
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(": ", Style::default().fg(theme::BLUE)),
            query,
            Span::styled("▏", Style::default().fg(theme::FG)),
        ])).style(Style::default().bg(theme::BG_DARK)),
        Rect { x: inner.x + 1, y: inner.y, width: inner.width.saturating_sub(2), height: 1 },
    );
    // Divider.
    if inner.height > 1 {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled("─".repeat(inner.width as usize), Style::default().fg(theme::BORDER_DIM)))),
            Rect { x: inner.x, y: inner.y + 1, width: inner.width, height: 1 },
        );
    }

    let sel = app.palette_index.min(results.len().saturating_sub(1));
    let list_top = inner.y + 2;
    for (i, item) in results.iter().enumerate() {
        let y = list_top + i as u16;
        if y >= inner.y + inner.height {
            break;
        }
        let selected = i == sel;
        let row = Rect { x: inner.x, y, width: inner.width, height: 1 };
        f.render_widget(Block::default().style(row_style(selected)), row);
        let mut spans = vec![
            selection_bar(selected, theme::BLUE),
            icon_chip(item.icon_letter, item.icon_color),
            Span::raw(" "),
            Span::styled(item.label.clone(), Style::default().fg(theme::FG)),
        ];
        let used: u16 = spans.iter().map(|s| s.width() as u16).sum();
        let hint_w = item.hint.chars().count() as u16 + 1;
        if inner.width > used + hint_w {
            spans.push(Span::styled(" ".repeat((inner.width - used - hint_w) as usize), row_style(selected)));
        }
        spans.push(Span::styled(item.hint, Style::default().fg(theme::FG_DIM)));
        f.render_widget(Paragraph::new(Line::from(spans)).style(row_style(selected)), row);
        zones.push(row, Action::RunPalette(i));
    }
}

/// Centered keybinding help modal (`?`).
pub fn help(f: &mut Frame, _app: &App, area: Rect, zones: &mut Zones) {
    zones.push(area, Action::CloseHelp);

    let bindings: [(&str, &str); 13] = [
        (":", "command palette"),
        ("^B", "toggle explorer"),
        ("[ / ]", "switch tab"),
        ("w / s / a", "workspace / settings / about"),
        ("p", "open plugin manager"),
        ("r / g / b", "expand recent files / git / keybindings"),
        ("j / k", "move selection"),
        ("⏎ / space", "open / toggle"),
        ("⇥ / ⇧⇥", "next / prev buffer"),
        ("2 / 3", "columns / grid layout"),
        ("i", "insert mode"),
        ("Esc", "close / normal mode"),
        ("?", "toggle this help"),
    ];

    let w = 60u16.min(area.width.saturating_sub(4));
    let rows = bindings.len().div_ceil(2) as u16;
    let h = rows + 4;
    let panel = centered(area, w, h.min(area.height.saturating_sub(2)));
    f.render_widget(Clear, panel);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme::BORDER))
        .style(Style::default().bg(theme::BG_DARK))
        .title(Line::from(Span::styled(" Keybindings ", Style::default().fg(theme::FG).add_modifier(Modifier::BOLD))));
    let inner = block.inner(panel);
    f.render_widget(block, panel);

    let close_rect = Rect { x: panel.x + panel.width.saturating_sub(3), y: panel.y, width: 1, height: 1 };
    f.render_widget(Paragraph::new(Span::styled("×", Style::default().fg(theme::FG_DIM))), close_rect);
    zones.push(close_rect, Action::CloseHelp);

    let col_w = inner.width / 2;
    for (i, (keys, desc)) in bindings.iter().enumerate() {
        let col = (i % 2) as u16;
        let rownum = (i / 2) as u16;
        let cx = inner.x + col * col_w;
        let cy = inner.y + rownum;
        if cy >= inner.y + inner.height {
            break;
        }
        let line = Line::from(vec![
            Span::styled(format!("{keys:<11}"), Style::default().fg(theme::CYAN)),
            Span::styled(*desc, Style::default().fg(theme::FG_DIM)),
        ]);
        f.render_widget(
            Paragraph::new(line).style(Style::default().bg(theme::BG_DARK)),
            Rect { x: cx, y: cy, width: col_w, height: 1 },
        );
    }
}
