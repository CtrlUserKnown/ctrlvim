//! The file-buffer view: a live, line-numbered window onto the engine's real
//! buffer. Text, cursor position, and mode all come from `ctrlvim_core` — this
//! is the editor surface, not a static display.
//!
//! For markdown files with live rendering on ([`App::md_render_active`]), each
//! line is decorated: markup is styled and concealed the way `glow` renders a
//! document — except the **cursor's line**, which is shown raw so you can edit
//! the markup. One source line is always one screen row, so the cursor/scroll
//! math is identical whether rendering is on or off.

use ctrlvim_markdown::{analyze, MdLine};
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::app::App;
use crate::theme;

pub fn screen(f: &mut Frame, app: &App, _file_idx: usize, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let lines = app.editor_lines();
    let (cur_line, cur_col) = app.editor_cursor();
    let gutter_w = lines.len().max(1).to_string().len().max(2) as u16;
    let text_x = area.x + gutter_w + 2; // gutter number + two spaces
    let content_w = (area.x + area.width).saturating_sub(text_x);
    let height = area.height as usize;

    // Scroll so the cursor line stays visible.
    let top = if cur_line >= height { cur_line - height + 1 } else { 0 };

    // Cursor color tracks the mode: green in insert, blue otherwise.
    let cursor_color = if app.editor_mode() == "i" { theme::GREEN } else { theme::BLUE };

    // Decorate the whole buffer once when live markdown rendering is on. Fence
    // state spans lines, so this needs the full source, not just the viewport.
    let md: Option<Vec<MdLine>> =
        app.md_render_active().then(|| analyze(&lines.join("\n")));

    for row in 0..height {
        let i = top + row;
        let y = area.y + row as u16;
        if i >= lines.len() {
            // Empty rows past end-of-buffer, Vim-style `~` markers.
            f.render_widget(
                Paragraph::new(Line::from(Span::styled("~", Style::default().fg(theme::BORDER)))),
                Rect { x: area.x, y, width: 1, height: 1 },
            );
            continue;
        }

        let raw = &lines[i];
        let gutter = format!("{:>w$}  ", i + 1, w = gutter_w as usize);
        let mut spans = vec![Span::styled(gutter, Style::default().fg(theme::BORDER))];
        match &md {
            Some(mdlines) => md_spans(&mdlines[i], i == cur_line, content_w, &mut spans),
            None => spans.push(Span::styled(raw.clone(), Style::default().fg(theme::FG))),
        }
        f.render_widget(
            Paragraph::new(Line::from(spans)).style(Style::default().bg(theme::BG)),
            Rect { x: area.x, y, width: area.width, height: 1 },
        );

        // Block cursor on the active line. The cursor line renders raw (markup
        // visible), so source columns line up with the screen 1:1.
        if i == cur_line {
            let cx = text_x + cur_col as u16;
            if cx < area.x + area.width {
                let ch = raw.chars().nth(cur_col).unwrap_or(' ');
                f.render_widget(
                    Paragraph::new(Span::styled(
                        ch.to_string(),
                        Style::default().fg(theme::BG).bg(cursor_color),
                    )),
                    Rect { x: cx, y, width: 1, height: 1 },
                );
            }
        }
    }
}

/// Append a rendered markdown line's spans. `reveal` (the cursor's line) shows
/// raw source; otherwise markup is concealed/replaced. `content_w` is the width
/// available after the gutter, used to fill rules and code-block backgrounds.
fn md_spans(line: &MdLine, reveal: bool, content_w: u16, out: &mut Vec<Span<'static>>) {
    // A horizontal rule spans the full width when concealed.
    if let Some(rule) = line.rule() {
        if reveal {
            out.push(Span::styled(rule.raw.clone(), theme::md_style(rule.kind)));
        } else {
            out.push(Span::styled("─".repeat(content_w as usize), theme::md_style(rule.kind)));
        }
        return;
    }

    let mut shown = 0usize;
    for seg in &line.segs {
        let text = if reveal { &seg.raw } else { &seg.display };
        if text.is_empty() {
            continue;
        }
        shown += text.chars().count();
        out.push(Span::styled(text.clone(), theme::md_style(seg.kind)));
    }

    // Extend the code-block background across the rest of the row.
    if line.code_block && (shown as u16) < content_w {
        let pad = content_w as usize - shown;
        out.push(Span::styled(" ".repeat(pad), Style::default().bg(theme::CODE_BG)));
    }
}
