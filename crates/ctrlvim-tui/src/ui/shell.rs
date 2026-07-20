//! The always-visible shell: the top tab bar and the bottom status line.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use ratatui::Frame;

use crate::app::{Action, App};
use crate::theme;

use super::Zones;

/// One tab per open buffer + a right-aligned `CHARVIM · TUI` wordmark.
pub fn tab_bar(f: &mut Frame, app: &App, area: Rect, zones: &mut Zones) {
    f.render_widget(
        Block::default().style(Style::default().bg(theme::BG_DARK)),
        area,
    );

    let mut spans: Vec<Span> = Vec::new();
    let mut x = area.x;

    for (i, buf) in app.buffers.iter().enumerate() {
        let active = i == app.active;
        let dot = if active { theme::GREEN } else { theme::BORDER };
        let text_color = if active { theme::FG } else { theme::FG_DIM };
        let weight = if active { Modifier::BOLD } else { Modifier::empty() };

        let start = x;
        // " ● label "
        let lead = " ● ";
        spans.push(Span::styled(lead, Style::default().fg(dot).bg(theme::BG_DARK)));
        x += lead.chars().count() as u16;

        let label = format!("{} ", buf.label);
        spans.push(Span::styled(
            label.clone(),
            Style::default().fg(text_color).bg(theme::BG_DARK).add_modifier(weight),
        ));
        x += label.chars().count() as u16;

        // Whole tab (except the × glyph) selects the buffer.
        let tab_w = x - start;
        zones.push(Rect { x: start, y: area.y, width: tab_w, height: 1 }, Action::SelectBuffer(i));

        if buf.closable() {
            let close = "× ";
            let cx = x;
            spans.push(Span::styled(close, Style::default().fg(theme::FG_DIM).bg(theme::BG_DARK)));
            zones.push(Rect { x: cx, y: area.y, width: 2, height: 1 }, Action::CloseBuffer(i));
            x += close.chars().count() as u16;
        }

        // Divider between tabs.
        spans.push(Span::styled("│", Style::default().fg(theme::BORDER_DIM).bg(theme::BG_DARK)));
        x += 1;
    }

    let wordmark = "CHARVIM · TUI ";
    let used = x - area.x;
    let pad = area.width.saturating_sub(used + wordmark.chars().count() as u16);
    if pad > 0 {
        spans.push(Span::styled(" ".repeat(pad as usize), Style::default().bg(theme::BG_DARK)));
    }
    spans.push(Span::styled(wordmark, Style::default().fg(theme::FG_DIM).bg(theme::BG_DARK)));

    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// Mode badge + git branch + buffer name on the left; keybinding hints and the
/// language tag on the right.
pub fn status_line(f: &mut Frame, app: &App, area: Rect) {
    f.render_widget(
        Block::default().style(Style::default().bg(theme::BG_DARK)),
        area,
    );

    // Mode is engine-owned while a file buffer is focused; COMMAND when the
    // palette is open; NORMAL for the dashboard/plugin shell.
    let (mode_label, mode_color) = if app.palette_open {
        ("COMMAND", theme::PURPLE)
    } else if app.active_file().is_some() {
        match app.editor_mode() {
            "i" => ("INSERT", theme::GREEN),
            "v" | "V" => ("VISUAL", theme::ORANGE),
            "\u{16}" => ("V-BLOCK", theme::ORANGE),
            _ => ("NORMAL", theme::BLUE),
        }
    } else {
        ("NORMAL", theme::BLUE)
    };

    let buf_label = app.active_buffer().label.clone();

    let mut left_spans = vec![Span::styled(
        format!(" {mode_label} "),
        Style::default().fg(theme::BG_DARK).bg(mode_color).add_modifier(Modifier::BOLD),
    )];
    if let Some(g) = &app.project.git {
        left_spans.push(Span::styled(
            format!("  {} ", g.branch),
            Style::default().fg(theme::CYAN).bg(theme::BG_DARK),
        ));
    }
    left_spans.push(Span::styled(
        format!(" {buf_label} "),
        Style::default().fg(theme::FG_DIM).bg(theme::BG_DARK),
    ));
    let left = Line::from(left_spans);

    let hints = [
        (": palette", theme::FG_DIM),
        ("^B explorer", theme::FG_DIM),
        ("[ ] tabs", theme::FG_DIM),
        ("j/k nav", theme::FG_DIM),
        ("⏎ open", theme::FG_DIM),
        ("⇥ buffers", theme::FG_DIM),
        ("? help", theme::FG_DIM),
        ("rust", theme::FG),
    ];
    let mut right_spans: Vec<Span> = Vec::new();
    for (i, (t, c)) in hints.iter().enumerate() {
        if i > 0 {
            right_spans.push(Span::styled("  ", Style::default().bg(theme::BG_DARK)));
        }
        right_spans.push(Span::styled(*t, Style::default().fg(*c).bg(theme::BG_DARK)));
    }
    right_spans.push(Span::styled(" ", Style::default().bg(theme::BG_DARK)));
    let right = Line::from(right_spans).right_aligned();

    f.render_widget(Paragraph::new(left.clone()), area);
    // Render the hints only in the space right of the left segment so they
    // never overwrite the mode/branch/buffer text.
    let left_w = left.width() as u16;
    if area.width > left_w {
        let right_area = Rect { x: area.x + left_w, width: area.width - left_w, ..area };
        f.render_widget(Paragraph::new(right), right_area);
    }
}
