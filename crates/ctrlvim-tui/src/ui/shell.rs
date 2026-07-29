//! The always-visible shell: the top tab bar and the bottom status line.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use ratatui::Frame;

use crate::app::{Action, App};
use crate::theme;

use super::Zones;

/// One tab per open buffer, filling the bar width.
pub fn tab_bar(f: &mut Frame, app: &App, area: Rect, zones: &mut Zones) {
    f.render_widget(
        Block::default().style(Style::default().bg(theme::bg_dark())),
        area,
    );

    let mut spans: Vec<Span> = Vec::new();
    let mut x = area.x;

    for (i, buf) in app.buffers.iter().enumerate() {
        let active = i == app.active;
        let dot = if active { theme::green() } else { theme::border() };
        let text_color = if active { theme::fg() } else { theme::fg_dim() };
        let weight = if active { Modifier::BOLD } else { Modifier::empty() };

        let start = x;
        // " ● label "
        let lead = " ● ";
        spans.push(Span::styled(lead, Style::default().fg(dot).bg(theme::bg_dark())));
        x += lead.chars().count() as u16;

        let label = format!("{} ", buf.label);
        spans.push(Span::styled(
            label.clone(),
            Style::default().fg(text_color).bg(theme::bg_dark()).add_modifier(weight),
        ));
        x += label.chars().count() as u16;

        // Unsaved-changes marker (engine holds the live value for the active tab).
        let dirty = if active { app.active_modified() } else { buf.modified };
        if dirty {
            let mark = "+ ";
            spans.push(Span::styled(mark, Style::default().fg(theme::orange()).bg(theme::bg_dark())));
            x += mark.chars().count() as u16;
        }

        // Whole tab (except the × glyph) selects the buffer.
        let tab_w = x - start;
        zones.push(Rect { x: start, y: area.y, width: tab_w, height: 1 }, Action::SelectBuffer(i));

        if buf.closable() {
            let close = "× ";
            let cx = x;
            spans.push(Span::styled(close, Style::default().fg(theme::fg_dim()).bg(theme::bg_dark())));
            zones.push(Rect { x: cx, y: area.y, width: 2, height: 1 }, Action::CloseBuffer(i));
            x += close.chars().count() as u16;
        }

        // Divider between tabs.
        spans.push(Span::styled("│", Style::default().fg(theme::border_dim()).bg(theme::bg_dark())));
        x += 1;
    }

    // Fill the rest of the bar with the tab-bar background.
    let used = x - area.x;
    let pad = area.width.saturating_sub(used);
    if pad > 0 {
        spans.push(Span::styled(" ".repeat(pad as usize), Style::default().bg(theme::bg_dark())));
    }

    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// Mode badge + git branch + buffer name on the left; keybinding hints and the
/// language tag on the right.
pub fn status_line(f: &mut Frame, app: &App, area: Rect) {
    f.render_widget(
        Block::default().style(Style::default().bg(theme::bg_dark())),
        area,
    );

    // While the engine's `:` command line is open it owns the whole status
    // line (the buffer already includes the leading `:` prefix).
    if let Some(cmd) = app.engine.cmdline() {
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(cmd, Style::default().fg(theme::fg()).bg(theme::bg_dark())),
                Span::styled("▏", Style::default().fg(theme::fg()).bg(theme::bg_dark())),
            ])),
            area,
        );
        return;
    }

    // Mode is engine-owned while a file buffer is focused; COMMAND when the
    // palette is open; NORMAL for the dashboard/plugin shell.
    let (mode_label, mode_color) = if app.palette_open {
        ("COMMAND", theme::purple())
    } else if app.is_file() {
        match app.editor_mode() {
            "i" => ("INSERT", theme::green()),
            "v" | "V" => ("VISUAL", theme::orange()),
            "\u{16}" => ("V-BLOCK", theme::orange()),
            _ => ("NORMAL", theme::blue()),
        }
    } else {
        ("NORMAL", theme::blue())
    };

    let buf_label = app.active_buffer().label.clone();

    let mut left_spans = vec![Span::styled(
        format!(" {mode_label} "),
        Style::default().fg(theme::bg_dark()).bg(mode_color).add_modifier(Modifier::BOLD),
    )];
    if let Some(g) = &app.project.git {
        left_spans.push(Span::styled(
            format!("  {} ", g.branch),
            Style::default().fg(theme::cyan()).bg(theme::bg_dark()),
        ));
    }
    left_spans.push(Span::styled(
        format!(" {buf_label} "),
        Style::default().fg(theme::fg_dim()).bg(theme::bg_dark()),
    ));
    if app.active_modified() {
        left_spans.push(Span::styled("[+] ", Style::default().fg(theme::orange()).bg(theme::bg_dark())));
    }
    // A pending `<leader>` chord (from the engine's typeahead buffer), then any
    // transient message (e.g. `:w` ack).
    let pending = app.engine.pending_display();
    if !pending.is_empty() {
        left_spans.push(Span::styled(format!(" {pending}… "), Style::default().fg(theme::purple()).bg(theme::bg_dark())));
    }
    if !app.message.is_empty() {
        left_spans.push(Span::styled(
            format!(" {} ", app.message),
            Style::default().fg(theme::fg_muted()).bg(theme::bg_dark()),
        ));
    }
    let left = Line::from(left_spans);

    let mut hints: Vec<(&str, _)> = vec![(": palette", theme::fg_dim())];
    // The drawer hint only shows when the drawer is enabled in the config.
    if app.config.drawer {
        hints.push(("^B drawer", theme::fg_dim()));
    }
    hints.push(("^⇥ tabs", theme::fg_dim()));
    hints.push(("? help", theme::fg_dim()));
    // Inline AI suggestions: shown only once they've been switched on, and
    // colored by state — a model that failed to load says so here rather than
    // silently never suggesting anything.
    if let Some(badge) = app.ai_badge() {
        let color = match app.ai_status() {
            Some(s) if s.is_failed() => theme::red(),
            _ => theme::purple(),
        };
        hints.push((badge, color));
    }
    // The language tag reports the buffer's real filetype — the one the
    // tree-sitter highlighter is using — and is blank when there isn't one.
    if let Some(ft) = app.editor_filetype() {
        hints.push((ft.name(), theme::fg()));
    }
    let mut right_spans: Vec<Span> = Vec::new();
    for (i, (t, c)) in hints.iter().enumerate() {
        if i > 0 {
            right_spans.push(Span::styled("  ", Style::default().bg(theme::bg_dark())));
        }
        right_spans.push(Span::styled(*t, Style::default().fg(*c).bg(theme::bg_dark())));
    }
    right_spans.push(Span::styled(" ", Style::default().bg(theme::bg_dark())));
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
