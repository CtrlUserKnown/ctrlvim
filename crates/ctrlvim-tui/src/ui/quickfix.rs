//! The quickfix pane — Vim's quickfix window, drawn as a strip along the
//! bottom of the editor area.
//!
//! Rows are `path:line  message`, severity-colored, with the engine's current
//! entry highlighted. Every row is clickable and dispatches the same
//! [`Action::QuickfixSelect`] that `j`/`k` + Enter do, so keyboard and mouse
//! stay in sync (the click-zone convention the rest of the UI follows).

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use ratatui::Frame;

use ctrlvim_core::QfKind;

use super::{row_style, selection_bar, titled_panel_with_right, Zones};
use crate::app::{Action, App};
use crate::theme;

/// Rows of entries the pane shows, plus its border.
const PANE_ROWS: u16 = 8;

/// Split `area`, returning `(editor, pane)` — the pane is `None` when it's
/// closed or there isn't room, and then the editor keeps the whole area.
pub fn split(app: &App, area: Rect) -> (Rect, Option<Rect>) {
    if !app.quickfix_open || area.height < PANE_ROWS + 4 {
        return (area, None);
    }
    let pane_h = PANE_ROWS.min(area.height / 2);
    let editor = Rect { height: area.height - pane_h, ..area };
    let pane = Rect { y: area.y + editor.height, height: pane_h, ..area };
    (editor, Some(pane))
}

pub fn screen(f: &mut Frame, app: &App, area: Rect, zones: &mut Zones) {
    let qf = app.engine.session.quickfix();
    let count = Line::from(Span::styled(
        format!("┤ {} entries ├", qf.len()),
        Style::default().fg(theme::fg_dim()).bg(theme::bg_dark()),
    ));
    let title = if qf.title().is_empty() { "QUICKFIX" } else { qf.title() };
    let inner = titled_panel_with_right(f, area, title, theme::orange(), Some(count));
    if inner.height == 0 || inner.width == 0 {
        return;
    }

    if qf.is_empty() {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "no entries",
                Style::default().fg(theme::fg_dim()),
            )))
            .style(Style::default().bg(theme::bg_dark())),
            inner,
        );
        return;
    }

    // Scroll so the selection stays visible.
    let cap = inner.height as usize;
    let sel = app.quickfix_index.min(qf.len() - 1);
    let start = if qf.len() <= cap {
        0
    } else if sel >= cap {
        (sel + 1 - cap).min(qf.len() - cap)
    } else {
        0
    };

    for (row, item) in qf.items().iter().skip(start).take(cap).enumerate() {
        let i = start + row;
        let selected = i == sel;
        let y = inner.y + row as u16;
        let rect = Rect { x: inner.x, y, width: inner.width, height: 1 };
        f.render_widget(Block::default().style(row_style(selected)), rect);

        // `path:line` is the part a reader scans, so it leads and is padded to
        // a column; the message follows in the severity's color.
        let location = format!(
            "{}:{}",
            item.path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default(),
            item.line + 1
        );
        let (tag, color) = match item.kind {
            QfKind::Error => ("E", theme::red()),
            QfKind::Warning => ("W", theme::orange()),
            QfKind::Info => ("I", theme::blue()),
            QfKind::Note => ("N", theme::cyan()),
            QfKind::Match => (" ", theme::green()),
        };
        let spans = vec![
            selection_bar(selected, theme::orange()),
            Span::styled(tag, Style::default().fg(color).add_modifier(Modifier::BOLD)),
            Span::raw(" "),
            Span::styled(format!("{location:<28}"), Style::default().fg(theme::blue())),
            Span::styled(item.text.clone(), Style::default().fg(theme::fg())),
        ];
        f.render_widget(Paragraph::new(Line::from(spans)).style(row_style(selected)), rect);
        zones.push(rect, Action::QuickfixSelect(i));
    }
}
