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

use super::{centered, file_chip, icon_chip, row_style, selection_bar, Zones};

/// Left-anchored file explorer drawer (Ctrl+B).
pub fn explorer(f: &mut Frame, app: &App, area: Rect, zones: &mut Zones) {
    zones.push(area, Action::CloseSidebar); // click-outside closes

    let w = 34u16.min(area.width);
    let panel = Rect { x: area.x, y: area.y, width: w, height: area.height };
    f.render_widget(Clear, panel);
    let block = Block::default()
        .borders(Borders::RIGHT)
        .border_style(Style::default().fg(theme::border()))
        .style(Style::default().bg(theme::bg_dark()));
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
    line_at!(y, Line::from(Span::styled("EXPLORER", Style::default().fg(theme::fg_dim()).add_modifier(Modifier::BOLD))));
    let close_rect = Rect { x: inner.x + inner.width.saturating_sub(2), y, width: 1, height: 1 };
    if y < bottom {
        f.render_widget(Paragraph::new(Span::styled("×", Style::default().fg(theme::fg_dim()))), close_rect);
        zones.push(close_rect, Action::CloseSidebar);
    }
    y += 2;

    // Search field: `/` drops into it; shows a hint otherwise.
    let search_line = if app.drawer_search {
        Line::from(vec![
            Span::styled("/", Style::default().fg(theme::blue())),
            Span::styled(app.drawer_query.clone(), Style::default().fg(theme::fg())),
            Span::styled("▏", Style::default().fg(theme::fg())),
        ])
    } else {
        Line::from(Span::styled("/ to search", Style::default().fg(theme::fg_dim())))
    };
    line_at!(y, search_line);
    y += 2;

    // Project root (the real cwd directory name).
    let root_name = app
        .root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| app.root.display().to_string());
    line_at!(y, Line::from(Span::styled(format!("▾ {root_name}"), Style::default().fg(theme::blue()).add_modifier(Modifier::BOLD))));
    y += 1;

    // File list, filtered by the `/` search query.
    let matches = app.drawer_matches();
    if matches.is_empty() {
        line_at!(y, Line::from(Span::styled("  no matches", Style::default().fg(theme::fg_dim()))));
        y += 1;
    }
    for &i in &matches {
        if y >= bottom {
            break;
        }
        let file = &app.project.recent_files[i];
        let selected = i == app.file_index;
        let row = Rect { x: inner.x, y, width: inner.width, height: 1 };
        f.render_widget(Block::default().style(row_style(selected)), row);
        let spans = vec![
            selection_bar(selected, theme::blue()),
            Span::raw(" "),
            file_chip(&file.icon, app.config.icons),
            Span::raw(" "),
            Span::styled(file.name.clone(), Style::default().fg(if selected { theme::fg() } else { theme::fg_muted() })),
        ];
        f.render_widget(Paragraph::new(Line::from(spans)).style(row_style(selected)), row);
        zones.push(row, Action::OpenFile(i));
        y += 1;
    }

    y += 1;
    line_at!(y, Line::from(Span::styled("GIT", Style::default().fg(theme::fg_dim()).add_modifier(Modifier::BOLD))));
    y += 1;
    if let Some(g) = &app.project.git {
        line_at!(y, Line::from(Span::styled(format!("  {}", g.branch), Style::default().fg(theme::purple()))));
        y += 1;
        line_at!(
            y,
            Line::from(vec![
                Span::styled(format!("↑{}", g.ahead), Style::default().fg(theme::green())),
                Span::styled(" · ", Style::default().fg(theme::fg_dim())),
                Span::styled(format!("~{}", g.modified), Style::default().fg(theme::orange())),
                Span::styled(" · ", Style::default().fg(theme::fg_dim())),
                Span::styled(format!("+{}", g.staged), Style::default().fg(theme::cyan())),
            ])
        );
    } else {
        line_at!(y, Line::from(Span::styled("not a repo", Style::default().fg(theme::fg_dim()))));
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
        .border_style(Style::default().fg(theme::border()))
        .style(Style::default().bg(theme::bg_dark()));
    let inner = block.inner(panel);
    f.render_widget(block, panel);

    // Input row: the command line itself — a `:` prompt and the typed query.
    let query = if app.palette_query.is_empty() {
        Span::styled("type a command…", Style::default().fg(theme::fg_dim()))
    } else {
        Span::styled(app.palette_query.clone(), Style::default().fg(theme::fg()))
    };
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(":", Style::default().fg(theme::blue()).add_modifier(Modifier::BOLD)),
            query,
            Span::styled("▏", Style::default().fg(theme::fg())),
        ])).style(Style::default().bg(theme::bg_dark())),
        Rect { x: inner.x + 1, y: inner.y, width: inner.width.saturating_sub(2), height: 1 },
    );
    // Divider.
    if inner.height > 1 {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled("─".repeat(inner.width as usize), Style::default().fg(theme::border_dim())))),
            Rect { x: inner.x, y: inner.y + 1, width: inner.width, height: 1 },
        );
    }

    let sel = app.palette_index.min(results.len().saturating_sub(1));
    let list_top = inner.y + 2;
    let rows = inner.height.saturating_sub(2) as usize; // visible list rows
    // Scroll so the selection stays visible even when the list is longer than
    // the panel (the command catalog + themes can exceed the 10-row window).
    let scroll = if rows > 0 && sel >= rows { sel - rows + 1 } else { 0 };
    for (row_i, i) in (scroll..results.len()).enumerate() {
        let y = list_top + row_i as u16;
        if y >= inner.y + inner.height {
            break;
        }
        let item = &results[i];
        let selected = i == sel;
        let row = Rect { x: inner.x, y, width: inner.width, height: 1 };
        f.render_widget(Block::default().style(row_style(selected)), row);
        let mut spans = vec![
            selection_bar(selected, theme::blue()),
            icon_chip(item.icon_letter, item.icon_color),
            Span::raw(" "),
            Span::styled(item.label.clone(), Style::default().fg(theme::fg())),
        ];
        let used: u16 = spans.iter().map(|s| s.width() as u16).sum();
        let hint_w = item.hint.chars().count() as u16 + 1;
        if inner.width > used + hint_w {
            spans.push(Span::styled(" ".repeat((inner.width - used - hint_w) as usize), row_style(selected)));
        }
        spans.push(Span::styled(item.hint, Style::default().fg(theme::fg_dim())));
        f.render_widget(Paragraph::new(Line::from(spans)).style(row_style(selected)), row);
        zones.push(row, Action::RunPalette(i));
    }
}

/// Centered "Save as" prompt for naming an unnamed buffer on write.
pub fn save_prompt(f: &mut Frame, app: &App, area: Rect, zones: &mut Zones) {
    zones.push(area, Action::CloseSavePrompt); // click-outside cancels
    let Some(name) = &app.save_prompt else { return };
    if area.width < 20 || area.height < 5 {
        return;
    }

    let w = 54u16.min(area.width.saturating_sub(4));
    let panel = centered(area, w, 3);
    f.render_widget(Clear, panel);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme::border()))
        .style(Style::default().bg(theme::bg_dark()))
        .title(Line::from(Span::styled(
            " Save as ",
            Style::default().fg(theme::green()).add_modifier(Modifier::BOLD),
        )));
    let inner = block.inner(panel);
    f.render_widget(block, panel);
    zones.push(panel, Action::None); // clicks on the panel don't cancel

    let shown = if name.is_empty() {
        Span::styled("filename…", Style::default().fg(theme::fg_dim()))
    } else {
        Span::styled(name.clone(), Style::default().fg(theme::fg()))
    };
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("  ", Style::default()),
            shown,
            Span::styled("▏", Style::default().fg(theme::fg())),
        ]))
        .style(Style::default().bg(theme::bg_dark())),
        Rect { x: inner.x, y: inner.y, width: inner.width, height: 1 },
    );
}

/// Centered keybinding help modal (`?`).
pub fn help(f: &mut Frame, _app: &App, area: Rect, zones: &mut Zones) {
    zones.push(area, Action::CloseHelp);

    let bindings: [(&str, &str); 16] = [
        ("␣e / ␣ff", "fuzzy file browser"),
        ("␣S / :Find", "find & replace in project"),
        ("n / :new", "new file"),
        ("␣w / ␣q", "write / write+quit"),
        (":w :q :wq", "ex write / quit"),
        (":", "command palette"),
        ("^⇥ / ^⇧⇥", "next / prev tab"),
        ("␣1 – ␣9", "jump to tab N"),
        ("w / s / a", "workspace / settings / about"),
        ("p", "open plugin manager"),
        ("g", "expand git"),
        ("j / k", "move selection"),
        ("⇥ / ⇧⇥", "cycle tabs"),
        ("i / Esc", "insert / normal mode"),
        (":source :lua", "run a script"),
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
        .border_style(Style::default().fg(theme::border()))
        .style(Style::default().bg(theme::bg_dark()))
        .title(Line::from(Span::styled(" Keybindings ", Style::default().fg(theme::fg()).add_modifier(Modifier::BOLD))));
    let inner = block.inner(panel);
    f.render_widget(block, panel);

    let close_rect = Rect { x: panel.x + panel.width.saturating_sub(3), y: panel.y, width: 1, height: 1 };
    f.render_widget(Paragraph::new(Span::styled("×", Style::default().fg(theme::fg_dim()))), close_rect);
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
            Span::styled(format!("{keys:<11}"), Style::default().fg(theme::cyan())),
            Span::styled(*desc, Style::default().fg(theme::fg_dim())),
        ]);
        f.render_widget(
            Paragraph::new(line).style(Style::default().bg(theme::bg_dark())),
            Rect { x: cx, y: cy, width: col_w, height: 1 },
        );
    }
}
