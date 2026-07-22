//! The file-buffer view: a live, line-numbered window onto the engine's real
//! buffer. Text, cursor position, and mode all come from `ctrlvim_core` — this
//! is the editor surface, not a static display.
//!
//! For markdown files with live rendering on ([`App::md_render_active`]), each
//! line is decorated: markup is styled and concealed the way `glow` renders a
//! document — except the **cursor's line**, which is shown raw so you can edit
//! the markup. One source line is always one screen row, so the cursor/scroll
//! math is identical whether rendering is on or off.

use ctrlvim_core::{Selection, VisualKind};
use ctrlvim_markdown::{analyze, MdLine};
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::app::App;
use crate::theme;

pub fn screen(f: &mut Frame, app: &App, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let lines = app.editor_lines();
    let (cur_line, cur_col) = app.editor_cursor();
    let selection = app.editor_selection();
    let gutter_w = lines.len().max(1).to_string().len().max(2) as u16;
    let text_x = area.x + gutter_w + 2; // gutter number + two spaces
    let content_w = (area.x + area.width).saturating_sub(text_x);
    let height = area.height as usize;

    // Scroll so the cursor line stays visible.
    let top = if cur_line >= height { cur_line - height + 1 } else { 0 };

    // Cursor color tracks the mode: green in insert, blue otherwise.
    let cursor_color = if app.editor_mode() == "i" { theme::green() } else { theme::blue() };

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
                Paragraph::new(Line::from(Span::styled("~", Style::default().fg(theme::border())))),
                Rect { x: area.x, y, width: 1, height: 1 },
            );
            continue;
        }

        let raw = &lines[i];
        let gutter = format!("{:>w$}  ", i + 1, w = gutter_w as usize);
        let mut spans = vec![Span::styled(gutter, Style::default().fg(theme::border()))];
        match &md {
            Some(mdlines) => md_spans(&mdlines[i], i == cur_line, content_w, &mut spans),
            None => spans.push(Span::styled(raw.clone(), Style::default().fg(theme::fg()))),
        }
        f.render_widget(
            Paragraph::new(Line::from(spans)).style(Style::default().bg(theme::bg())),
            Rect { x: area.x, y, width: area.width, height: 1 },
        );

        // Visual-mode selection: paint the selection background over the
        // already-rendered cells so the user can see what they're highlighting.
        // set_style repaints the background without touching the glyphs, so
        // this works regardless of markdown decoration on the row.
        if let Some(sel) = selection {
            if let Some((lo, hi)) = selection_cols(&sel, i, raw.chars().count()) {
                let right = area.x + area.width;
                let x0 = text_x + lo as u16;
                if x0 < right {
                    let x1 = (text_x + hi as u16).min(right);
                    let w = x1.saturating_sub(x0);
                    if w > 0 {
                        f.buffer_mut().set_style(
                            Rect { x: x0, y, width: w, height: 1 },
                            Style::default().bg(theme::selection()),
                        );
                    }
                }
            }
        }

        // `hlsearch`: paint a background over every match of the active search
        // pattern on this row (dark text on the search color, Vim-style).
        for (lo, hi) in app.editor_search_matches(i) {
            let x0 = text_x + lo as u16;
            let right = area.x + area.width;
            if x0 >= right {
                continue;
            }
            let x1 = (text_x + hi as u16).min(right);
            let w = x1.saturating_sub(x0);
            if w > 0 {
                f.buffer_mut().set_style(
                    Rect { x: x0, y, width: w, height: 1 },
                    Style::default().fg(theme::bg()).bg(theme::search()),
                );
            }
        }

        // Block cursor on the active line. The cursor line renders raw (markup
        // visible), so source columns line up with the screen 1:1.
        if i == cur_line {
            let cx = text_x + cur_col as u16;
            if cx < area.x + area.width {
                let ch = raw.chars().nth(cur_col).unwrap_or(' ');
                f.render_widget(
                    Paragraph::new(Span::styled(
                        ch.to_string(),
                        Style::default().fg(theme::bg()).bg(cursor_color),
                    )),
                    Rect { x: cx, y, width: 1, height: 1 },
                );
            }
        }
    }
}

/// The half-open source-column range `[lo, hi)` selected on line `i`, or `None`
/// if the line is outside the selection. `len` is the line's character count.
///
/// Charwise flows across line ends (first/middle rows extend one cell past the
/// text to show the line break is included); linewise takes the whole row;
/// blockwise takes the min..max column band on every row in range. Columns are
/// interpreted 1:1 with screen cells, matching the cursor overlay.
fn selection_cols(sel: &Selection, i: usize, len: usize) -> Option<(usize, usize)> {
    if i < sel.start.line || i > sel.end.line {
        return None;
    }
    match sel.kind {
        // Highlight the whole line; at least one cell so empty lines still show.
        VisualKind::Line => Some((0, (len + 1).max(1))),
        VisualKind::Block => {
            let lo = sel.start.col.min(sel.end.col);
            let hi = sel.start.col.max(sel.end.col);
            Some((lo, hi + 1))
        }
        VisualKind::Char => {
            let range = if sel.start.line == sel.end.line {
                (sel.start.col, sel.end.col + 1)
            } else if i == sel.start.line {
                (sel.start.col, len + 1)
            } else if i == sel.end.line {
                (0, sel.end.col + 1)
            } else {
                (0, len + 1)
            };
            Some(range)
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
        out.push(Span::styled(" ".repeat(pad), Style::default().bg(theme::code_bg())));
    }
}
