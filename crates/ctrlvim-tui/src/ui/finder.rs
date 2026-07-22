//! The full-screen fuzzy file browser (telescope `file_browser` style): a big
//! bordered results box over a "File Browser" prompt box. Browse the current
//! directory, type to fuzzy-filter, drill into subdirectories, open files.
//!
//! The results list is bottom-anchored (like `fzf`), so the highlighted row and
//! the search prompt sit together near the bottom of the screen.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::app::{Action, App};
use crate::theme;

use super::{centered, icon_chip, row_style, selection_bar, Zones};

pub fn screen(f: &mut Frame, app: &App, area: Rect, zones: &mut Zones) {
    let Some(finder) = &app.finder else { return };
    // Click anywhere outside the popup closes it.
    zones.push(area, Action::CloseFinder);
    if area.width < 24 || area.height < 8 {
        return;
    }

    // A centered floating popup rather than a full-screen takeover.
    let w = area.width.saturating_sub(8).min(96);
    let h = area.height.saturating_sub(4).min(24);
    let popup = centered(area, w, h);
    f.render_widget(Clear, popup);

    // Results box on top, a 3-row prompt box beneath it.
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(3)])
        .split(popup);
    // Swallow clicks inside the popup so they don't fall through to close.
    zones.push(popup, Action::None);

    let matches = app.finder_matches();
    let total = finder.entries.len();

    // --- results box -------------------------------------------------------
    let dir_title = display_dir(app);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme::border()))
        .style(Style::default().bg(theme::bg()))
        .title(Line::from(Span::styled(
            format!(" {dir_title} "),
            Style::default().fg(theme::blue()).add_modifier(Modifier::BOLD),
        )).centered());
    let list = block.inner(rows[0]);
    f.render_widget(block, rows[0]);

    if list.height > 0 && list.width > 2 {
        // Bottom-anchored + scrolled so the selection stays visible.
        let cap = list.height as usize;
        let sel = finder.selected.min(matches.len().saturating_sub(1));
        // Window of `cap` matches ending at (or past) the selection.
        let start = if matches.len() <= cap {
            0
        } else if sel >= cap {
            (sel + 1 - cap).min(matches.len() - cap)
        } else {
            0
        };
        let window: Vec<usize> = matches.iter().copied().skip(start).take(cap).collect();
        // First row so the last entry hugs the box bottom.
        let first_y = list.y + (cap.saturating_sub(window.len())) as u16;

        for (row, &ei) in window.iter().enumerate() {
            let entry = &finder.entries[ei];
            let match_idx = start + row;
            let selected = match_idx == sel;
            let y = first_y + row as u16;
            let rect = Rect { x: list.x, y, width: list.width, height: 1 };
            f.render_widget(Block::default().style(row_style(selected)), rect);

            let name_color = if entry.is_dir { theme::blue() } else { theme::fg() };
            let mut spans = vec![
                selection_bar(selected, theme::blue()),
                icon_chip(entry.icon_letter, entry.icon_color),
                Span::raw(" "),
                Span::styled(entry.name.clone(), Style::default().fg(name_color)),
            ];
            // Right-aligned `perms  size  mtime` metadata column.
            let meta = vec![
                Span::styled(entry.perms.clone(), Style::default().fg(theme::fg_dim())),
                Span::raw("  "),
                Span::styled(format!("{:>6}", entry.size), Style::default().fg(theme::green())),
                Span::raw("  "),
                Span::styled(format!("{:>12}", entry.mtime), Style::default().fg(theme::blue())),
            ];
            let used: u16 = spans.iter().map(|s| s.width() as u16).sum();
            let meta_w: u16 = meta.iter().map(|s| s.width() as u16).sum();
            if list.width > used + meta_w + 1 {
                spans.push(Span::styled(
                    " ".repeat((list.width - used - meta_w) as usize),
                    row_style(selected),
                ));
                spans.extend(meta);
            }
            f.render_widget(Paragraph::new(Line::from(spans)).style(row_style(selected)), rect);
            zones.push(rect, Action::RunFinder(match_idx));
        }
    }

    // --- prompt box --------------------------------------------------------
    let pblock = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme::border()))
        .style(Style::default().bg(theme::bg()))
        .title(Line::from(Span::styled(" File Browser ", Style::default().fg(theme::fg_dim()))).centered());
    let pinner = pblock.inner(rows[1]);
    f.render_widget(pblock, rows[1]);
    // Swallow clicks on the prompt itself so they don't close the finder.
    zones.push(rows[1], Action::None);

    if pinner.height > 0 && pinner.width > 4 {
        let prompt = Line::from(vec![
            Span::styled(" 🔍 ", Style::default().fg(theme::blue())),
            Span::styled(finder.query.clone(), Style::default().fg(theme::fg())),
            Span::styled("▏", Style::default().fg(theme::fg())),
        ]);
        f.render_widget(Paragraph::new(prompt), pinner);
        // Right side: match count, or — when nothing matches a typed name — a
        // hint that pressing Enter creates that file here.
        let (right, color) = if matches.is_empty() && !finder.query.trim().is_empty() {
            (format!("⏎ create “{}” ", finder.query.trim()), theme::green())
        } else {
            (format!("{} / {} ", matches.len(), total), theme::fg_dim())
        };
        let cw = right.chars().count() as u16;
        if pinner.width > cw {
            let cx = pinner.x + pinner.width - cw;
            f.render_widget(
                Paragraph::new(Line::from(Span::styled(right, Style::default().fg(color)))),
                Rect { x: cx, y: pinner.y, width: cw, height: 1 },
            );
        }
    }
}

/// The directory shown in the results-box title: relative to the project root
/// when inside it (`./sub/dir`), otherwise the absolute path.
fn display_dir(app: &App) -> String {
    let Some(finder) = &app.finder else { return "/".into() };
    match finder.dir.strip_prefix(&app.root) {
        Ok(rel) if rel.as_os_str().is_empty() => "./".into(),
        Ok(rel) => format!("./{}/", rel.display()),
        Err(_) => format!("{}/", finder.dir.display()),
    }
}
