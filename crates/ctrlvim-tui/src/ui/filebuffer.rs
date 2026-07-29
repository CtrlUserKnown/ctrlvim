//! The file-buffer view: a live, line-numbered window onto the engine's real
//! buffer. Text, cursor position, and mode all come from `ctrlvim_core` — this
//! is the editor surface, not a static display.
//!
//! For markdown files with live rendering on ([`App::md_render_active`]), each
//! line is decorated: markup is styled and concealed the way `glow` renders a
//! document — except the **cursor's line**, which is shown raw so you can edit
//! the markup. One source line is always one screen row, so the cursor/scroll
//! math is identical whether rendering is on or off.

use ctrlvim_core::{char_width, width_upto, HlSpan, Selection, Suggestion, VisualKind};
use ctrlvim_markdown::{analyze, MdLine};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
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

    // Where the viewport starts, in *screen* rows (a closed fold is one row).
    // The mouse wheel moves `view_top`; clamping it against the cursor here
    // means keyboard movement scrolls the view without the app having to track
    // the viewport, and the cursor can never be off-screen.
    app.set_viewport_rows(height);
    let cur_row = app.screen_line_of(cur_line);
    let top_row = app
        .view_top()
        .clamp(cur_row.saturating_sub(height.saturating_sub(1)), cur_row);
    // The buffer lines these rows actually show: `visible[row]`, never
    // `top + row`, because folds make those differ.
    let visible = app.visible_lines(top_row, height, lines.len());
    let first_line = visible.first().copied().unwrap_or(0);

    // Cursor color tracks the mode: green in insert, blue otherwise.
    let cursor_color = if app.editor_mode() == "i" { theme::green() } else { theme::blue() };

    // The inline AI suggestion, if the model has one for this cursor.
    let ghost = app.suggestion();

    // Decorate the whole buffer once when live markdown rendering is on. Fence
    // state spans lines, so this needs the full source, not just the viewport.
    let md: Option<Vec<MdLine>> =
        app.md_render_active().then(|| analyze(&lines.join("\n")));
    // Tree-sitter highlights over the buffer-line span the viewport covers —
    // wider than `height` when folds are closed inside it — indexed from
    // `first_line`.
    let span_len = visible.last().map(|last| last + 1 - first_line).unwrap_or(0);
    let hl = app.editor_highlights(&lines, first_line, span_len);

    for row in 0..height {
        let y = area.y + row as u16;
        let Some(&i) = visible.get(row) else {
            // Empty rows past end-of-buffer, Vim-style `~` markers.
            f.render_widget(
                Paragraph::new(Line::from(Span::styled("~", Style::default().fg(theme::border())))),
                Rect { x: area.x, y, width: 1, height: 1 },
            );
            continue;
        };

        let raw = &lines[i];
        let gutter = format!("{:>w$}  ", i + 1, w = gutter_w as usize);
        let mut spans = vec![Span::styled(gutter, Style::default().fg(theme::border()))];
        // A closed fold draws one summary row instead of its lines.
        if let Some(fold) = app.fold_head_at(i) {
            spans.push(Span::styled(
                ctrlvim_core::fold_text(fold, raw),
                Style::default().fg(theme::fg_dim()).bg(theme::code_bg()),
            ));
            f.render_widget(
                Paragraph::new(Line::from(spans)).style(Style::default().bg(theme::code_bg())),
                Rect { x: area.x, y, width: area.width, height: 1 },
            );
            continue;
        }
        match md.as_ref().map(|mdlines| mdlines.get(i)) {
            Some(Some(mdline)) => md_spans(mdline, i == cur_line, content_w, &mut spans),
            // Markdown is on, but this row has no decoration. That happens on
            // the buffer's phantom trailing line: a `Buffer`'s rope always ends
            // in `\n` (its documented `'eol'` invariant), so `lines()` reports a
            // final empty line that `analyze` deliberately does not — see its
            // `split_lines`. The two counts therefore differ by one as soon as
            // the buffer ends in a blank line, which any edit at the end
            // produces. An empty line has nothing to decorate, so rendering it
            // plain is not a fallback but the right answer; going through
            // `get` also means a renderer can never panic on the mismatch.
            Some(None) => syntax_spans(raw, &[], &mut spans),
            None => syntax_spans(
                raw,
                hl.get(i - first_line).map(Vec::as_slice).unwrap_or(&[]),
                &mut spans,
            ),
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
                // Selection bounds are *character* indices; the screen is in
                // cells, which differ as soon as the line holds a wide glyph.
                let x0 = text_x + width_upto(&raw, lo) as u16;
                if x0 < right {
                    let x1 = (text_x + width_upto(&raw, hi) as u16).min(right);
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
            let x0 = text_x + width_upto(&raw, lo) as u16;
            let right = area.x + area.width;
            if x0 >= right {
                continue;
            }
            let x1 = (text_x + width_upto(&raw, hi) as u16).min(right);
            let w = x1.saturating_sub(x0);
            if w > 0 {
                f.buffer_mut().set_style(
                    Rect { x: x0, y, width: w, height: 1 },
                    Style::default().fg(theme::bg()).bg(theme::search()),
                );
            }
        }

        // Inline AI suggestion ("ghost text"): the model's proposal, drawn dim.
        // It is *not* in the buffer, so it's painted over the finished row
        // rather than spliced into the spans above — which keeps the cursor,
        // selection, and search math untouched.
        if let Some(sug) = ghost {
            let right = area.x + area.width;
            if i == sug.anchor.line {
                let x = text_x + width_upto(raw, sug.anchor.col) as u16;
                // The rest of the real line is redrawn *after* the ghost, so a
                // suggestion offered mid-line pushes the existing text right
                // instead of covering it.
                ghost_head(f, sug.head(), &raw[byte_at(raw, sug.anchor.col)..], x, y, right);
            }
            if let Some(line) = tail_row_for(sug, i) {
                ghost_row(f, line, text_x, y, right);
            }
        }

        // Block cursor on the active line. The cursor line renders raw (markup
        // visible), so source columns line up with the screen 1:1.
        if i == cur_line {
            let cx = text_x + width_upto(&raw, cur_col) as u16;
            if cx < area.x + area.width {
                // With ghost text under the cursor the block shows the model's
                // first character, not the buffer's: that cell now belongs to
                // the suggestion.
                let ch = ghost_char_at(ghost, cur_line, cur_col)
                    .or_else(|| raw.chars().nth(cur_col))
                    .unwrap_or(' ');
                // A wide glyph needs a two-cell cursor, or the block sits on
                // half a character and the other half keeps the normal colors.
                let w = char_width(ch) as u16;
                let w = w.min((area.x + area.width).saturating_sub(cx)).max(1);
                f.render_widget(
                    Paragraph::new(Span::styled(
                        ch.to_string(),
                        Style::default().fg(theme::bg()).bg(cursor_color),
                    )),
                    Rect { x: cx, y, width: w, height: 1 },
                );
            }
        }
    }
}

// --- inline AI suggestions --------------------------------------------------
//
// Ghost text is proposed, not present: it lives in the engine's suggestion
// state, never in the buffer, and disappears on the next keystroke unless it is
// accepted. Rendering therefore paints *over* a finished row instead of taking
// part in the span building above, so nothing about how a line is highlighted,
// selected, or searched changes when a suggestion happens to be showing.

/// How ghost text is drawn: dim and italic, so it never reads as real code.
fn ghost_style() -> Style {
    Style::default()
        .fg(theme::fg_muted())
        .bg(theme::bg())
        .add_modifier(Modifier::ITALIC)
}

/// Draw the first line of a suggestion at the cursor, followed by whatever the
/// real line had after the cursor.
///
/// Redrawing `rest` is what makes a mid-line suggestion readable: without it
/// the ghost would sit *on top of* the text after the cursor rather than
/// appearing to push it aside, which is how the same feature behaves in a
/// proportional editor.
fn ghost_head(f: &mut Frame, ghost: &str, rest: &str, x: u16, y: u16, right: u16) {
    if ghost.is_empty() || x >= right {
        return;
    }
    let spans = vec![
        Span::styled(ghost.to_string(), ghost_style()),
        Span::styled(rest.to_string(), Style::default().fg(theme::fg()).bg(theme::bg())),
    ];
    f.render_widget(
        Paragraph::new(Line::from(spans)),
        Rect { x, y, width: right - x, height: 1 },
    );
}

/// Draw one continuation row of a multi-line suggestion.
///
/// These rows sit where the buffer's *next* lines are drawn, and cover them:
/// one source line is one screen row here (the invariant the fold and scroll
/// maths depend on), so there is nowhere to push the real text down to. The
/// row is cleared to the code background first, so it reads as an overlay
/// panel rather than as buffer content that mysteriously changed — and it is
/// gone as soon as the suggestion is accepted or dismissed. Set
/// `[ai] max_lines = 1` for strictly inline suggestions.
fn ghost_row(f: &mut Frame, text: &str, x: u16, y: u16, right: u16) {
    if x >= right {
        return;
    }
    let area = Rect { x, y, width: right - x, height: 1 };
    f.render_widget(Block::default().style(Style::default().bg(theme::code_bg())), area);
    f.render_widget(
        Paragraph::new(Span::styled(
            text.to_string(),
            ghost_style().bg(theme::code_bg()),
        )),
        area,
    );
}

/// The continuation line a suggestion puts on buffer row `line`, if any.
/// Line 0 of the suggestion belongs to the anchor's own row, so row `n` below
/// the anchor shows suggestion line `n + 1`.
fn tail_row_for(sug: &Suggestion, line: usize) -> Option<&str> {
    let below = line.checked_sub(sug.anchor.line + 1)?;
    sug.text.split('\n').nth(below + 1)
}

/// Byte offset of column `col` in `s`, clamped into the string and back to the
/// nearest character boundary — engine columns are byte offsets, and slicing a
/// line mid-character would panic.
fn byte_at(s: &str, col: usize) -> usize {
    let mut col = col.min(s.len());
    while col > 0 && !s.is_char_boundary(col) {
        col -= 1;
    }
    col
}

/// The first character of a suggestion anchored exactly at `(line, col)` — the
/// glyph the block cursor should show, since that cell now belongs to the
/// ghost text rather than to the buffer.
fn ghost_char_at(ghost: Option<&Suggestion>, line: usize, col: usize) -> Option<char> {
    let sug = ghost?;
    if sug.anchor.line != line || sug.anchor.col != col {
        return None;
    }
    sug.head().chars().next()
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

/// Append one source line's spans, cut at its tree-sitter highlight ranges.
/// With no highlights (unsupported filetype, or a line with nothing to style)
/// this is the plain single-span rendering.
///
/// Spans arrive in char columns and in order; anything between them — and any
/// range running past the line, which a stale cache could produce mid-edit —
/// falls back to the default text color rather than dropping characters.
fn syntax_spans(raw: &str, hl: &[HlSpan], out: &mut Vec<Span<'static>>) {
    let plain = Style::default().fg(theme::fg());
    if hl.is_empty() {
        out.push(Span::styled(raw.to_string(), plain));
        return;
    }
    let chars: Vec<char> = raw.chars().collect();
    let mut col = 0usize;
    for span in hl {
        let start = span.start.clamp(col, chars.len());
        let end = span.end.clamp(start, chars.len());
        if start > col {
            out.push(Span::styled(chars[col..start].iter().collect::<String>(), plain));
        }
        if end > start {
            out.push(Span::styled(
                chars[start..end].iter().collect::<String>(),
                theme::syn_style(span.kind),
            ));
        }
        col = end;
    }
    if col < chars.len() {
        out.push(Span::styled(chars[col..].iter().collect::<String>(), plain));
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
